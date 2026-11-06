/**
 * 价格维护 API 层（后端 `backend/src/business/erp/price_admin_handler.rs` + `price_handler.rs`，
 * SO-32 建表 / SO-33 取价 / SO-34 维护页）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `sales-return.ts` / `shipment.ts` 同一口径：价格是独立后端模块（自己的
 * handler / model / 迁移 V0270 + V0308），按模块边界单独成文件，
 * 页面只 `import '@/api/business/erp/customer-price'`，`index.ts` 一行不动。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 客户 | `customer_id` / `customer_code` / `customer_name` | `AgreedRes` / `TierRes`（LEFT JOIN 补齐，软删客户为 null） |
 * | 物料 | `material_id` / `material_code` / `material_name` / `material_spec` | 同上 |
 * | 单价 | `price`（**JSON 里是字符串**，`NUMERIC(18,4)`） | 全仓金额口径一致 |
 * | 起订量 | `min_qty` | `0` = 不限制 |
 * | 档位区间 | `min_qty`（含）/ `max_qty`（**不含**，null = 无上限） | ARCH-7 §15.3.3 ② |
 * | 有效期 | `effective_from` / `effective_to`（**两端都含**，null = 长期） | 与取价侧 `date_covered` 同口径 |
 * | 档位类型 | `tier_type`：`TIER` / `PROMO` | `ck_biz_price_tier_type` |
 * | 取价来源 | `source`：`AGREED`/`TIER_CUSTOMER`/`TIER_COMMON`/`STANDARD`/`NONE` | `price_dto::SOURCES` |
 *
 * # 五个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **取价 `qty` 必填**（SO-33：缺省按 0 会静默命中 min_qty=0 的低价档）；`customer_id` 可空
 *    （不传 = 跳过协议价与客户专属阶梯）；
 * 3. **`NONE` 是 200 不是 404**：`price = null` 是合法业务结果（让销售手填），
 *    页面上必须先判 `source === 'NONE'` 再判 price，别把 null 当错误；
 * 4. **`source_id` 在 `STANDARD` 时是物料 id**（不是价格行 id），前端不要拿它当价格行主键；
 * 5. **编辑档位「清空无上限」要传 `-1`**：JSON 的 `null` 与「字段缺省」反序列化后都是
 *    `None`（= 不改），后端约定 `max_qty: -1` 表示清空成无上限（见 `UpdateTierReq::merge`）。
 */
import { del, get, post, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp/price';

/** 价格状态（`ACTIVE` 参与取价 / `INACTIVE` 停用但保留历史） */
export type PriceStatus = 'ACTIVE' | 'INACTIVE';

/** 档位类型：`TIER` 量价阶梯 / `PROMO` 促销 */
export type TierType = 'TIER' | 'PROMO';

/** 取价来源（与 `price_dto::SOURCES` 逐字对应，顺序 = 优先级从高到低） */
export type PriceSource =
  | 'AGREED'
  | 'TIER_CUSTOMER'
  | 'TIER_COMMON'
  | 'STANDARD'
  | 'NONE';

/** 金额 / 数量：后端 `NUMERIC(18,4)` 在 JSON 里是字符串，但 `t-input-number` 给数字 */
export type DecimalText = string | number;

// ==================== 客户协议价 ====================

/** 协议价一行（列表 / 新建 / 编辑同构） */
export interface CustomerPriceRow {
  id: string;
  customer_id: string;
  /** 客户被物理删除时为 null（LEFT JOIN 不吞行） */
  customer_code: string | null;
  customer_name: string | null;
  material_id: string;
  material_code: string | null;
  material_name: string | null;
  material_spec: string | null;
  /** 含税协议单价 */
  price: DecimalText;
  /** 起订量（0 = 不限制） */
  min_qty: DecimalText;
  /** 生效日（含） */
  effective_from: string;
  /** 失效日（含；null = 长期有效） */
  effective_to: string | null;
  status: PriceStatus;
  remark: string | null;
  created_at: string;
  updated_at: string;
}

/** 协议价分页筛选 */
export interface CustomerPriceFilter {
  customer_id?: string;
  material_id?: string;
  status?: PriceStatus | '';
  /** 客户 / 物料的编码或名称（模糊） */
  keyword?: string;
  page?: number;
  page_size?: number;
}

/** 新建协议价 */
export interface CreateCustomerPriceReq {
  customer_id: string;
  material_id: string;
  price: DecimalText;
  /** 缺省 0 */
  min_qty?: DecimalText;
  /** 缺省 = 今天（CST） */
  effective_from?: string | null;
  /** 缺省 = 长期有效 */
  effective_to?: string | null;
  /** 缺省 ACTIVE */
  status?: PriceStatus;
  remark?: string | null;
}

/** 编辑协议价（部分更新：不传 = 不改；空串 = 清空可空列） */
export type UpdateCustomerPriceReq = Partial<CreateCustomerPriceReq>;

/** 丢掉空串 / null / undefined（后端把 `?x=` 当「等于空串」过滤，必须根本不发） */
function asParams<T extends object>(query: T): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === '') {
      continue;
    }
    out[key] = value;
  }
  return out;
}

/** 协议价分页（`GET /business/erp/price/agreed/page`，权限 `business:erp:cust-price:list`） */
export function fetchCustomerPricePage(
  params: CustomerPriceFilter,
): Promise<PageData<CustomerPriceRow>> {
  return get<PageData<CustomerPriceRow>>(`${BASE}/agreed/page`, asParams(params));
}

/** 新建协议价（`POST /business/erp/price/agreed`，权限 `:create`） */
export function createCustomerPrice(
  payload: CreateCustomerPriceReq,
): Promise<CustomerPriceRow> {
  return post<CustomerPriceRow>(`${BASE}/agreed`, payload);
}

/** 编辑协议价（`PUT /business/erp/price/agreed/{id}`，权限 `:update`） */
export function updateCustomerPrice(
  id: string,
  payload: UpdateCustomerPriceReq,
): Promise<CustomerPriceRow> {
  return put<CustomerPriceRow>(`${BASE}/agreed/${id}`, payload);
}

/** 软删协议价（`DELETE /business/erp/price/agreed/{id}`，权限 `:delete`） */
export function deleteCustomerPrice(id: string): Promise<void> {
  return del<void>(`${BASE}/agreed/${id}`);
}

// ==================== 阶梯价 / 促销价 ====================

/** 档位一行 */
export interface PriceTierRow {
  id: string;
  material_id: string;
  material_code: string | null;
  material_name: string | null;
  material_spec: string | null;
  /** null = 通用档位（适用所有客户） */
  customer_id: string | null;
  customer_code: string | null;
  customer_name: string | null;
  tier_type: TierType;
  /** 区间下界（含） */
  min_qty: DecimalText;
  /** 区间上界（**不含**；null = 无上限） */
  max_qty: DecimalText | null;
  price: DecimalText;
  effective_from: string;
  effective_to: string | null;
  status: PriceStatus;
  remark: string | null;
  created_at: string;
  updated_at: string;
}

/** 档位分页筛选 */
export interface PriceTierFilter {
  material_id?: string;
  customer_id?: string;
  tier_type?: TierType | '';
  status?: PriceStatus | '';
  keyword?: string;
  page?: number;
  page_size?: number;
}

/** 新建档位 */
export interface CreatePriceTierReq {
  material_id: string;
  /** 缺省 / null = 通用档位 */
  customer_id?: string | null;
  /** 缺省 TIER */
  tier_type?: TierType;
  min_qty: DecimalText;
  /** 缺省 null = 无上限 */
  max_qty?: DecimalText | null;
  price: DecimalText;
  effective_from?: string | null;
  effective_to?: string | null;
  status?: PriceStatus;
  remark?: string | null;
}

/** 编辑档位（部分更新；`max_qty: -1` = 清空成无上限） */
export interface UpdatePriceTierReq
  extends Partial<Omit<CreatePriceTierReq, 'customer_id'>> {
  /** 空串 = 改成通用档位 */
  customer_id?: string | null;
}

/** 档位分页（`GET /business/erp/price/tier/page`，权限 `business:erp:price-tier:list`） */
export function fetchPriceTierPage(params: PriceTierFilter): Promise<PageData<PriceTierRow>> {
  return get<PageData<PriceTierRow>>(`${BASE}/tier/page`, asParams(params));
}

/** 新建档位（`POST /business/erp/price/tier`，权限 `:create`） */
export function createPriceTier(payload: CreatePriceTierReq): Promise<PriceTierRow> {
  return post<PriceTierRow>(`${BASE}/tier`, payload);
}

/** 编辑档位（`PUT /business/erp/price/tier/{id}`，权限 `:update`） */
export function updatePriceTier(
  id: string,
  payload: UpdatePriceTierReq,
): Promise<PriceTierRow> {
  return put<PriceTierRow>(`${BASE}/tier/${id}`, payload);
}

/** 软删档位（`DELETE /business/erp/price/tier/{id}`，权限 `:delete`） */
export function deletePriceTier(id: string): Promise<void> {
  return del<void>(`${BASE}/tier/${id}`);
}

// ==================== 取价解析（SO-33） ====================

/** 取价解析入参（`qty` **必传**，`customer_id` 可空） */
export interface PriceResolveQuery {
  customer_id?: string;
  material_id: string;
  qty: DecimalText;
  /** `YYYY-MM-DD`，缺省 = 今天（CST） */
  date?: string;
}

/** 取价解析出参：`NONE` 时 `price` / `source_id` 都是 null（200，不是错误） */
export interface PriceResolveRes {
  price: DecimalText | null;
  source: PriceSource;
  source_id: string | null;
}

/**
 * 取价解析（`GET /business/erp/price/resolve`，权限 `business:erp:price:resolve`）。
 *
 * 5 级确定性：`AGREED`（客户协议价）→ `TIER_CUSTOMER`（客户专属阶梯）→
 * `TIER_COMMON`（通用阶梯）→ `STANDARD`（物料标准售价）→ `NONE`（未找到，手填）。
 */
export function resolvePrice(query: PriceResolveQuery): Promise<PriceResolveRes> {
  return get<PriceResolveRes>(`${BASE}/resolve`, asParams(query));
}
