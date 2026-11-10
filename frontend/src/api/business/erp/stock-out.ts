/**
 * 出库单 API 层（DC-13 后端 `backend/src/business/erp/stockout_{dto,handler,model}.rs`）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * `index.ts` 已经是 3800+ 行的仓储域总契约（并且是**唯一契约来源**的兼容壳基线）。
 * DC-13 的出库单是独立后端模块（自己的 handler / DTO / 迁移 V0203），按模块边界
 * 单独成文件，页面只 `import '@/api/business/erp/stock-out'`；`index.ts` 一行不动，
 * 别人正在改它时不会互相踩。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 单号 | `out_no` | `stockout_dto.rs::StockOutRes::out_no` |
 * | 类型 | `out_type` + `out_type_label` | 后端给中文名，前端不再维护第二份译名 |
 * | 状态 | `status` + `status_label` | 同上 |
 * | 明细成本 | `items[].unit_cost` / `items[].amount` | 过账时写快照，草稿为 0 |
 * | 可去状态 | `next_statuses` / `legal_edges` | **按钮置灰只读这一份**，前端不写第二份状态机 |
 *
 * # 三个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML →
 *    前端报「响应体格式不符合」）；
 * 2. **详情是扁平的**：`StockOutDetailRes` 用 `#[serde(flatten)]` 把 `StockOutRes` 摊进顶层，
 *    所以 `detail.out_no` 直接可取（不像收货单那样有 `header` 包一层）；
 * 3. **建单明细不传单价 / 金额 / 单位**：`StockOutItemReq` 只有
 *    `material_id / qty / location_id / lot_no / remark`，成本由服务端按账上移动加权单价
 *    的**真实差额**算（`RETURNING before.amount - after.amount`）。类型上就没有那几个字段，
 *    防御性传了也会被 serde 忽略 —— 与其默默忽略，不如在类型层面禁止。
 */

import { del, get, patch, post, put, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/** 丢掉空串 / null / undefined（后端把 `?keyword=` 当「等于空串」过滤，会查出 0 行） */
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

/**
 * 出库类型（与 `stockout_base::OUT_TYPES` / `ck_biz_so_type` 逐字一致）。
 *
 * - `ISSUE` 领用：车间 / 部门领料，凭证借 **6401 主营业务成本**；
 * - `SCRAP` 报废：借 **1901 待处理财产损溢**，**必须填事由**；
 * - `LOSS` 盘亏：借 **1901 待处理财产损溢**，**必须填事由**；
 * - `OTHER` 其他：借 6401。
 */
export type StockOutType = 'ISSUE' | 'SCRAP' | 'LOSS' | 'OTHER';

/**
 * 出库单状态（与 `stockout_base::STATUS_*` 逐字一致）。
 *
 * ```text
 * DRAFT ──提交──→ PENDING_POST ──过账──→ POSTED ──作废（必填原因）──→ CANCELLED
 *   │                 │                    └──────────作废───────────→ CANCELLED
 *   └──过账───────────┴──→ POSTED
 *   └──作废（必填原因）───→ CANCELLED
 * ```
 */
export type StockOutStatus = 'DRAFT' | 'PENDING_POST' | 'POSTED' | 'CANCELLED';

/** 列表行（逐字抄 `StockOutRes`） */
export interface StockOutRow {
  /** 主键 */
  id: string;
  /** 出库单号 `OUT-yyyyMMdd-000001` */
  out_no: string;
  /** 出库类型 */
  out_type: StockOutType | string;
  /** 出库类型中文名（后端给：领用 / 报废 / 盘亏 / 其他） */
  out_type_label: string;
  /** 出库仓库 id */
  warehouse_id: string;
  /** 出库日期 `YYYY-MM-DD` */
  out_date: string;
  /** 状态 */
  status: StockOutStatus | string;
  /** 状态中文名（后端给：草稿 / 待过账 / 已过账 / 已作废） */
  status_label: string;
  /** 数量合计 */
  total_qty: string;
  /**
   * 成本结转金额合计。
   *
   * **草稿恒为 0**：出库金额取账上移动加权单价的真实扣减额，只有过账那一刻才知道，
   * 所以列表 / 详情都要标「过账后才有值」。
   */
  total_amount: string;
  /** 成本结转凭证 id（未过账 / 零成本存量出库为 `null`） */
  voucher_id: string | null;
  /** 出库事由（领用用途 / 报废原因 / 盘亏说明） */
  reason: string | null;
  /** 备注 */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 制单人（用户 id） */
  create_by: string | null;
  /** 制单人姓名（AUD-02 起后端批量 join 下发；不再把 id 当显示名） */
  create_by_name?: string | null;
  /**
   * 成本对象（ARCH-9 B1-10；`null` = 未挂 = 存量口径）。
   *
   * **只有领用（`ISSUE`）单能挂**：后端对报废 / 盘亏挂对象直接 400
   * （资产损失必进 1901，料只按 `ISSUE` 归集）。
   */
  cost_object_id?: string | null;
  /** 成本对象号 `CB-yyyyMMdd-NNNNNN`（实时取；未挂 / 已删 → `null`） */
  cost_object_no?: string | null;
  /**
   * **过账借方科目**编码（后端 `stockout_base::debit_account_of_doc` 复算下发）。
   *
   * 挂对象 + 领用 ⇒ `4001.01` 生产成本-直接材料；否则 `6401` / `1901`。
   * 前端**不自己算过账科目**，只拿它显示（表单里那句「选完借方会变」的预览除外，
   * 见 `stock-out-logic.ts::debitAccountHint`，那边注明了两边同源）。
   */
  debit_account_code?: string;
  /** 借方科目名称（`生产成本-直接材料` / `主营业务成本` / `待处理财产损溢`） */
  debit_account_name?: string;
}

/** 出库表单「成本对象」下拉项（`GET /erp/stock-out/cost-object-options`，只列 OPEN） */
export interface StockOutCostObjectOption {
  /** 成本对象 id */
  id: string;
  /** 对象号 `CB-yyyyMMdd-NNNNNN` */
  object_no: string;
  /** 产品 id */
  product_id: string;
  /** 产品编码（实时 join） */
  product_code: string | null;
  /** 产品名称（实时 join） */
  product_name: string | null;
  /** 计划数量（字符串） */
  plan_qty: string;
  /** 开工日期 `YYYY-MM-DD` */
  start_date: string | null;
}

/** 明细行（逐字抄 `StockOutItemRes`） */
export interface StockOutItemLine {
  /** 主键 */
  id: string;
  /** 行号（1 起） */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 物料编码（后端详情补的展示名，可能为 `null` = 物料已删） */
  material_code: string | null;
  /** 物料名称 */
  material_name: string | null;
  /** 计量单位 id（基础单位，服务端按物料决定） */
  unit_id: string;
  /** 出库数量 */
  qty: string;
  /** 过账时的移动加权单价快照（草稿为 `0`） */
  unit_cost: string;
  /** 过账时账上真实扣减金额（草稿为 `0`） */
  amount: string;
  /** 库位账行（`biz_inventory_location.id`，可空 = 未分配库位桶） */
  location_id: string | null;
  /** 批号 */
  lot_no: string | null;
  /** 行备注 */
  remark: string | null;
}

/**
 * 详情（`StockOutDetailRes`：单头 `#[serde(flatten)]` 摊平 + 明细 + 动作）。
 *
 * ⚠️ 与收货单不同，**没有 `header` 包一层** —— 直接 `detail.out_no`。
 */
export interface StockOutDetail extends StockOutRow {
  /** 明细 */
  items: StockOutItemLine[];
  /** 过账时间（RFC3339） */
  posted_at: string | null;
  /** 作废时间（RFC3339） */
  cancelled_at: string | null;
  /** 作废原因 */
  cancel_reason: string | null;
  /**
   * 当前状态能去哪些状态（后端算好下发）。
   *
   * **详情抽屉的按钮只由它驱动**：前端不写第二份状态机，后端加了边 / 收紧了边，
   * 页面自动跟着变（`DRAFT → CANCELLED` 这种口子在不在，一键改后端即可）。
   */
  next_statuses: string[];
  /** 写死的边表（`[(from, to)]`，与 `next_statuses` 同源，可供调试 / 单测对照） */
  legal_edges: Array<[string, string]>;
}

/** 建单 / 改单的一行明细（**没有单价 / 金额 / 单位**：由服务端按物料与账上均价决定） */
export interface StockOutItemReq {
  /** 物料 id */
  material_id: string;
  /** 出库数量（> 0，0 数量 400） */
  qty: string;
  /** 库位账行（`biz_inventory_location.id`，可空） */
  location_id?: string | null;
  /** 批号（可空） */
  lot_no?: string | null;
  /** 行备注（可空） */
  remark?: string | null;
}

/** 建单请求体（`POST /erp/stock-out`） */
export interface CreateStockOutReq {
  /** 出库类型（缺省 `ISSUE`） */
  out_type: StockOutType;
  /** 出库仓库（必填） */
  warehouse_id: string;
  /** 出库日期 `YYYY-MM-DD`（缺省今天 CST） */
  out_date: string;
  /** 出库事由（`SCRAP` / `LOSS` 必填：必须挂 1901，报文里会说理由） */
  reason?: string | null;
  /** 备注（≤ 500 字） */
  remark?: string | null;
  /**
   * 成本对象（ARCH-9 §4.7.1；**只有领用 `ISSUE` 能挂**）。
   *
   * - 缺省 / `null` = 不挂 ⇒ 借方按原逻辑 `6401`（报废 / 盘亏恒 `1901`）；
   * - 传 id = 挂到该成本对象 ⇒ 借方 `4001.01` 生产成本-直接材料。
   *
   * 改单是**整单覆盖**：不传 = 清空（与 `reason` / `remark` 同口径）。
   * 对象必须是 `OPEN`，否则 409 + `66057`。
   */
  cost_object_id?: string | null;
  /** 明细（1–200 行） */
  items: StockOutItemReq[];
}

/** 改单请求体（`PUT /erp/stock-out/{id}`，**仅 DRAFT**，整单覆盖明细） */
export type UpdateStockOutReq = CreateStockOutReq;

/**
 * 状态流转请求体（`PATCH /erp/stock-out/{id}/status`）。
 *
 * - `PENDING_POST`：提交待过账（交接位，配合 `erp.separation_of_duties.post`）；
 * - `DRAFT`：从待过账撤回；
 * - `POSTED`：过账（同一事务里三本账负向 + 成本结转凭证 + 负库存闸）；
 * - `CANCELLED`：作废，**`reason` 必填**（进状态日志与红冲凭证摘要）。
 */
export interface StockOutStatusReq {
  /** 目标状态 */
  status: StockOutStatus;
  /** 原因（作废必填） */
  reason?: string | null;
}

/** 出库单分页（`GET /erp/stock-out/page`，`list` 元素是扁平 `StockOutRes`） */
export function fetchStockOutPage(query: {
  /** 单号 / 事由 / 备注（模糊） */
  keyword?: string;
  status?: StockOutStatus | string;
  out_type?: StockOutType | string;
  warehouse_id?: string;
  /** 出库日期起 `YYYY-MM-DD` */
  date_from?: string;
  /** 出库日期止 `YYYY-MM-DD` */
  date_to?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<StockOutRow>> {
  return get<PageData<StockOutRow>>(`${BASE}/stock-out/page`, asParams(query));
}

/** 出库单详情（`GET /erp/stock-out/{id}`，**扁平**：没有 `header` 包一层） */
export function fetchStockOutDetail(id: string): Promise<StockOutDetail> {
  return get<StockOutDetail>(`${BASE}/stock-out/${id}`);
}

/**
 * 领料单可选的成本对象（`GET /erp/stock-out/cost-object-options`，**只列 `OPEN`**）。
 *
 * 为什么不用成本对象页那个列表接口（`GET /cost-object?status=OPEN`）：
 * 那条走 `business:erp:cost-object:list`，V0378 只授给财务角色；
 * 开领料单的是仓管（`stock-out:*`），去查它会 403 → 下拉永远空。
 * 本接口按 `business:erp:stock-out:list` 授权，仓管开单就能选。
 *
 * 上限 200 条（后端 `COST_OBJECT_OPTION_LIMIT`），超量靠 `filterable` 搜索。
 */
export function fetchStockOutCostObjectOptions(params: { keyword?: string } = {}): Promise<
  StockOutCostObjectOption[]
> {
  return get<StockOutCostObjectOption[]>(`${BASE}/stock-out/cost-object-options`, asParams(params));
}

/**
 * 建单（`POST /erp/stock-out`）。
 *
 * 返回的是**完整详情**（与收货单同款），前端不用再发一次 GET。
 */
export function createStockOut(payload: CreateStockOutReq): Promise<StockOutDetail> {
  return post<StockOutDetail>(`${BASE}/stock-out`, payload);
}

/** 改单（`PUT /erp/stock-out/{id}`，**仅 DRAFT**，整单覆盖明细） */
export function updateStockOut(id: string, payload: UpdateStockOutReq): Promise<StockOutDetail> {
  return put<StockOutDetail>(`${BASE}/stock-out/${id}`, payload);
}

/** 删除（`DELETE /erp/stock-out/{id}`，**仅 DRAFT**，软删） */
export function deleteStockOut(id: string): Promise<void> {
  return del<void>(`${BASE}/stock-out/${id}`);
}

/**
 * 状态流转（`PATCH /erp/stock-out/{id}/status`）。
 *
 * 失败口径（页面要原样展示报文，不要只写「操作失败」）：
 * - `409 + 66034` 负库存拦截：报文点名物料编码/名称 + 该库位桶**可用量** + 仓库；
 * - `409 + 66035` 作废时过账流水缺行（拒绝按现价估算）；
 * - `400 + 66002` 引用不存在 / `400 + 10001` 参数错。
 */
export function updateStockOutStatus(
  id: string,
  payload: StockOutStatusReq,
): Promise<StockOutDetail> {
  return patch<StockOutDetail>(`${BASE}/stock-out/${id}/status`, payload);
}
