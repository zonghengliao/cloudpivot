/**
 * 取价解析 API（后端 `backend/src/business/erp/price_handler.rs`，SO-33 / ARCH-7 §15.3.3）。
 *
 * # 为什么单独一个文件
 *
 * 与 `receipt-refund.ts` / `sales-return.ts` 同一口径：`index.ts` 已是数千行的总契约，
 * 取价是独立后端模块（`price_handler` / `price_dto` / `price_model`），按模块边界自成文件。
 *
 * # 一个必须记住的口径（后端文件头第 16-18 行原文）
 *
 * **填单只准用 `price_excl`（不含税）**：`price` 对协议价 / 阶梯价是**含税**价，
 * 直接填进销售订单行会让单价虚高一个税率（120 vs 106.19），税额再算一遍 → 整单金额全错。
 *
 * # `NONE` 是正常结果
 *
 * 五级取价全没命中 → 200 + `source = 'NONE'`，不是 404：没价是业务事实，
 * 让销售员手填并提示「未找到协议价」。物料 / 客户不存在才是 404。
 */
import { get } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/**
 * 命中层级（5 值，逐字抄 `price_dto::SOURCES`）。
 *
 * 顺序即优先级：`AGREED`（客户协议价）→ `TIER_CUSTOMER`（客户专属阶梯）→
 * `TIER_COMMON`（通用阶梯）→ `STANDARD`（物料标准售价）→ `NONE`（都没价）。
 */
export type PriceSource = 'AGREED' | 'TIER_CUSTOMER' | 'TIER_COMMON' | 'STANDARD' | 'NONE';

/** 五级取价的展示顺序与中文名（页面「为什么是这个价」一栏用） */
export const PRICE_SOURCES: { value: PriceSource; label: string; hint: string }[] = [
  { value: 'AGREED', label: '客户协议价', hint: '与该客户单独签的协议价（含税）' },
  { value: 'TIER_CUSTOMER', label: '客户专属阶梯', hint: '只对这家客户生效的数量阶梯价（含税）' },
  { value: 'TIER_COMMON', label: '通用阶梯', hint: '按数量走的通用阶梯价（含税）' },
  { value: 'STANDARD', label: '物料标准售价', hint: '物料档案上的标准售价（**不含税**）' },
  { value: 'NONE', label: '未取到价', hint: '上面四级都没有可用价 → 由销售手填' },
];

/** 命中层级中文名（`NONE` 也返回「未取到价」，不返回空串） */
export function priceSourceLabel(source: string | null | undefined): string {
  return PRICE_SOURCES.find((item) => item.value === source)?.label ?? source ?? '未知';
}

/** 取价结果（后端 `price_dto::PriceResolveRes`） */
export interface PriceResolveResult {
  /** 命中价（协议价 / 阶梯价是**含税**；标准售价是**不含税** —— 看 `tax_inclusive`） */
  price: string | null;
  /** 命中层级 */
  source: PriceSource | string;
  /** 命中行主键（`STANDARD` 时是物料 id；`NONE` 时 null） */
  source_id: string | null;
  /** `price` 是否含税（false = 该值已是不含税，与 `price_excl` 相等） */
  tax_inclusive: boolean;
  /** 折算后的**不含税**单价 —— **填订单行用这个值**；`NONE` 时 null */
  price_excl: string | null;
  /** 折算用的税率（百分数 0–100，13 = 13%）；`NONE` 时 null */
  tax_rate: string | null;
}

/** 取价入参（后端 `price_dto::PriceResolveQuery`） */
export interface PriceResolveQuery {
  /** 客户 id（选填：不传 = 跳过协议价与客户专属阶梯） */
  customer_id?: string;
  /** 物料 id（**必填**） */
  material_id: string;
  /** 订购量（**必填**，≥ 0） */
  qty: number | string;
  /** 取价基准日 `YYYY-MM-DD`（选填，缺省 = 今天北京时间） */
  date?: string;
}

/**
 * 取价解析（`GET /business/erp/price/resolve`，权限 `business:erp:price:resolve`）。
 *
 * 只读：不写表、不锁行、不过账、不落库（结果不缓存 —— 每次回答「此刻按规则应该是多少」）。
 */
export function resolvePrice(query: PriceResolveQuery): Promise<PriceResolveResult> {
  const params: Record<string, unknown> = { material_id: query.material_id, qty: query.qty };
  if (query.customer_id) {
    params.customer_id = query.customer_id;
  }
  if (query.date) {
    params.date = query.date;
  }
  return get<PriceResolveResult>(`${BASE}/price/resolve`, params);
}
