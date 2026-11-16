/**
 * 在途库存 API 层（后端 `backend/app/src/business/erp/inventory_transit/`，
 * M2-04 查询 / M2-05 对账；M2-07 消费前两个只读面，M2-07b 补对账面）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `shipment.ts` / `stock-out.ts` 同一口径：`index.ts` 已是 5400+ 行的总契约，
 * 而在途是独立后端模块（`inventory_transit`，自己的 handler / model / 迁移 V0420~V0422），
 * 按模块边界单独成文件，页面只 `import '@/api/business/erp/inventory-transit'`。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 在途量 | `qty_in_transit` | `TransitRes`（PG **生成列** `shipped_qty - received_qty`） |
 * | 累计发货量 | `shipped_qty` | M2-02 发货过账写入 |
 * | 已签收量 | `received_qty` | M2-03 签收确认推进 |
 * | 账龄 | `age_days` | PG 算的 `CURRENT_DATE - ship_date`（跨时区不差一天） |
 * | 状态 | `status` + `status_label` | 后端给中文名，前端不再维护第二份译名 |
 *
 * # 三个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端路由宏不含前缀，漏了会命中 SPA fallback 返回 HTML
 *    → 前端报「响应体格式不符合」）；
 * 2. **`status` 留空 ≠ 不加条件**：后端把「留空」当默认值 `IN_TRANSIT`
 *    （一次「查在途」不能把已签收的历史行翻出来）。想看历史必须**显式**选 `RECEIVED`；
 * 3. **数量是 `Decimal`，出参可能是字符串**（`"6.0000"`）。一律过
 *    `views/business/erp/inventory-transit/transit-logic.ts::formatQty` / `shared/money.toNumber`，
 *    别直接 `+`（`"6.0000" + 1` 是字符串拼接）。
 *
 * # 只读
 *
 * 本模块**没有任何写接口**：在途行的产生（发货过账）与结转（签收确认）分别由
 * `POST /business/erp/shipment/{id}/status`（过账）与 `POST /business/erp/shipment/{id}/receive`
 * （签收）负责。页面因此不挂 `create/update/delete` 按钮。
 */
import { get, type PageData } from '@/api/request';

/** 接口前缀（后端注解不含 `/api`） */
const BASE = '/api/business/erp';

/**
 * 在途行状态域（3 值，逐字抄 `shipment_transit::STATUS_*` / V0420 的 `ck_*`）。
 *
 * | 编码 | 中文 | 谁写 |
 * |------|------|------|
 * | `IN_TRANSIT` | 在途（未签收） | M2-02 发货过账 upsert |
 * | `RECEIVED` | 已签收 | M2-03 签收确认（整单签收） |
 * | `CANCELLED` | 已作废 | 发货单作废时的在途行处理 |
 */
export type TransitStatus = 'IN_TRANSIT' | 'RECEIVED' | 'CANCELLED';

/**
 * 一行在途明细（逐字抄 `inventory_transit::dto::TransitRes`）。
 *
 * 主数据名字（`customer_*` / `warehouse_*` / `material_*` / `unit_name`）由后端**批量补齐**，
 * 主数据软删后为 `null` —— 历史在途行照常可读，只是不再显示已删主数据的名字
 * （AGENTS.md 约定 13：业务展示类实时 join，不存快照）。
 */
export interface TransitRow {
  /** 在途行主键 */
  id: string;
  /** 发货单 id */
  shipment_id: string;
  /** 发货单号（发货单软删后 `null`） */
  ship_no: string | null;
  /** 客户 id（客户信息随发货单头走，发货单软删后 `null`） */
  customer_id: string | null;
  /** 客户编码（客户软删后 `null`） */
  customer_code: string | null;
  /** 客户名称（客户软删后 `null`） */
  customer_name: string | null;
  /** 发货仓库 id */
  warehouse_id: string;
  /** 仓库编码（仓库软删后 `null`） */
  warehouse_code: string | null;
  /** 仓库名称（仓库软删后 `null`） */
  warehouse_name: string | null;
  /** 物料 id */
  material_id: string;
  /** 物料编码（物料软删后 `null`） */
  material_code: string | null;
  /** 物料名称（物料软删后 `null`） */
  material_name: string | null;
  /** 规格型号 */
  spec: string | null;
  /** 计量单位名 */
  unit_name: string | null;
  /** 发货日期（`yyyy-MM-dd`） */
  ship_date: string;
  /** 在途天数 = `CURRENT_DATE - ship_date`（PG 算，前端不再算一遍） */
  age_days: number;
  /** 累计发货数量（上限） */
  shipped_qty: number | string;
  /** 已签收数量 */
  received_qty: number | string;
  /** **当前在途量**（PG 生成列，任何写入路径都写不出「不符」的值） */
  qty_in_transit: number | string;
  /** 状态编码 */
  status: TransitStatus;
  /** 状态中文名（后端给） */
  status_label: string;
  /** 全部签收时间（`RECEIVED` 才有值，RFC3339） */
  received_at: string | null;
  /** 备注（签收 / 作废原因等） */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 更新时间（RFC3339） */
  updated_at: string;
}

/**
 * 一行在途汇总（按 **物料 × 发货仓** 分组，逐字抄 `TransitSummaryRow`）。
 *
 * 汇总恒为**未签收在途**（`status = 'IN_TRANSIT'`）：把已签收 / 已作废混进来就不是在途了。
 */
export interface TransitSummaryRow {
  /** 物料 id */
  material_id: string;
  /** 物料编码（物料软删后 `null`） */
  material_code: string | null;
  /** 物料名称（物料软删后 `null`） */
  material_name: string | null;
  /** 规格型号 */
  spec: string | null;
  /** 计量单位名 */
  unit_name: string | null;
  /** 发货仓库 id */
  warehouse_id: string;
  /** 仓库编码（仓库软删后 `null`） */
  warehouse_code: string | null;
  /** 仓库名称（仓库软删后 `null`） */
  warehouse_name: string | null;
  /** 未签收在途量合计 */
  qty_in_transit: number | string;
  /** 发货量合计 */
  shipped_qty: number | string;
  /** 已签收量合计 */
  received_qty: number | string;
  /** 涉及的发货单张数（`count(DISTINCT shipment_id)`） */
  shipments: number;
  /** 最早一张发货单的发货日期（`yyyy-MM-dd`） */
  first_ship_date: string;
  /** 最早那批货的在途天数 */
  max_age_days: number;
}

/**
 * 在途汇总出参（逐字抄 `TransitSummaryRes`）。
 *
 * ⚠️ **`total_*` 不受 `rows` 截断影响**（后端另一条聚合查询、同一个 WHERE）：
 * `truncated = true` 只表示「行没给全」，合计仍是全量的 —— 页面**不许**把 `rows`
 * 自己加一遍当合计（那是第二套账，且截断时必然算小）。
 */
export interface TransitSummary {
  /** 汇总行（按在途量倒序；超过 500 行会截断） */
  rows: TransitSummaryRow[];
  /** 本次返回的汇总行数 */
  row_count: number;
  /** 是否被截断（`rows` 到了上限，还有别的 物料×仓库 组合没返回） */
  truncated: boolean;
  /** 全部匹配组的未签收在途量合计（不受截断影响） */
  total_qty_in_transit: number | string;
  /** 全部匹配组涉及的发货单张数 */
  total_shipments: number;
  /** 全部匹配组的在途行数 */
  total_lines: number;
}

/** 明细筛选 / 分页入参（后端 `TransitFilter`，字段名逐字一致） */
export interface TransitQuery {
  /** 物料 id（精确 UUID） */
  material_id?: string;
  /** 发货仓库 id（精确 UUID） */
  warehouse_id?: string;
  /** 客户 id（精确 UUID；经发货单头关联） */
  customer_id?: string;
  /** 发货单 id（精确 UUID） */
  shipment_id?: string;
  /** 状态（`''` / 不传 = 后端默认 `IN_TRANSIT`） */
  status?: TransitStatus | '';
  /** 发货日期下界（`yyyy-MM-dd`，含当天） */
  ship_date_from?: string;
  /** 发货日期上界（`yyyy-MM-dd`，含当天） */
  ship_date_to?: string;
  /** 页码（从 1 起） */
  page?: number;
  /** 每页条数（0 → 默认 20，上限 200） */
  page_size?: number;
}

/** 汇总筛选入参（后端 `TransitSummaryFilter`：没有 `status`，恒为未签收） */
export interface TransitSummaryQuery {
  /** 物料 id */
  material_id?: string;
  /** 发货仓库 id */
  warehouse_id?: string;
  /** 客户 id */
  customer_id?: string;
}

/**
 * 在途明细分页（`GET /business/erp/inventory-transit/page`，权限
 * `business:erp:inventory-transit:list`）。
 *
 * 空串一律**不发**（后端把 `?status=` 当「等于空串」过滤，会查出 0 行）——
 * 拼参由 `transit-logic.buildTransitQuery` 负责，本函数只做一次兜底 `asParams`。
 */
export function fetchTransitPage(query: TransitQuery): Promise<PageData<TransitRow>> {
  return get<PageData<TransitRow>>(`${BASE}/inventory-transit/page`, asParams(query));
}

/**
 * 按 物料 × 发货仓 汇总未签收在途（`GET /business/erp/inventory-transit/summary`，
 * 同一个权限码）。
 */
export function fetchTransitSummary(query: TransitSummaryQuery = {}): Promise<TransitSummary> {
  return get<TransitSummary>(`${BASE}/inventory-transit/summary`, asParams(query));
}

// ==================== 对账（M2-05 接口 · M2-07b 页面） ====================

/**
 * 对账差异码（逐字抄 `inventory_transit::reconcile` 的 `ISSUE_*` 常量）。
 *
 * 前端**只用来上色**（`danger` / `warning`）：中文文案一律取后端 `issue_labels`
 * ——差异文案是业务口径（`issue_label`），前端再抄一份必然有一天跟后端漂。
 */
export type TransitReconcileIssue =
  | 'QTY_MISMATCH'
  | 'MISSING_TRANSIT'
  | 'ORPHAN_TRANSIT'
  | 'IDENTITY_BROKEN'
  | 'MULTI_WAREHOUSE'
  | 'CANCELLED_ONLY';

/**
 * 一行对账明细（`发货单 × 物料` 粒度，逐字抄 `TransitReconcileRow`）。
 *
 * # 三侧数量 + 差额都由后端给
 *
 * | 语义 | 字段 | 口径 |
 * |------|------|------|
 * | 发货总量 | `item_qty` | 发货单明细 `Σ qty`（SSOT） |
 * | 已出 | `received_qty` | 在途账的已签收数量 |
 * | 在途 | `qty_in_transit` | 在途账的未签收量（PG 生成列） |
 * | 差额 | `diff` | `item_qty − (received_qty + qty_in_transit)`；`0` = 这组对平 |
 *
 * `diff` **不许前端自己减**：差额的符号与口径（含作废行怎么算）是后端
 * `reconcile::ComboRow::diff` 的事，前端再算一遍 = 第二套账。
 */
export interface TransitReconcileRow {
  /** 发货单 id */
  shipment_id: string;
  /** 发货单号（历史行为空） */
  ship_no: string | null;
  /** 发货单状态（`POSTED`；历史行可能为空） */
  shipment_status: string | null;
  /** 发货单是否已软删（明细与在途行都还在 ⇒ 对账照样看得见） */
  shipment_deleted: boolean;
  /** 发货日期（`yyyy-MM-dd`；历史行为空） */
  ship_date: string | null;
  /** 物料 id */
  material_id: string;
  /** 物料编码（物料软删后 `null`） */
  material_code: string | null;
  /** 物料名称 */
  material_name: string | null;
  /** 规格型号 */
  spec: string | null;
  /** 计量单位名 */
  unit_name: string | null;
  /** 发货仓库 id（`null` = 这组没有在途行） */
  warehouse_id: string | null;
  /** 仓库编码（仓库软删后 `null`） */
  warehouse_code: string | null;
  /** 仓库名称 */
  warehouse_name: string | null;
  /** 发货总量（发货单明细 Σqty，SSOT） */
  item_qty: number | string;
  /** 在途账记的发货量（非作废行） */
  shipped_qty: number | string;
  /** 已出（已签收数量） */
  received_qty: number | string;
  /** 在途量（未签收） */
  qty_in_transit: number | string;
  /** 作废行的发货量（不参与对账式，只做诊断） */
  cancelled_qty: number | string;
  /** 差额 = 发货总量 −（已出 + 在途）；后端算，`0` = 对平 */
  diff: number | string;
  /** 差异码（空数组 = 对平） */
  issues: string[];
  /** 差异码的中文说明（与 `issues` 同下标；**页面只用这个**） */
  issue_labels: string[];
  /** 是否对平（`issues` 为空） */
  matched: boolean;
}

/**
 * 对账汇总（逐字抄 `TransitReconcileTotals`）。
 *
 * `diff = item_qty − (received_qty + in_transit_qty)`：整体恒等式成不成立看它，
 * 页面**不自己减**（合计与差额同源，减出来对不上就说明前端在算第二本账）。
 */
export interface TransitReconcileTotals {
  /** 比对的组数（受扫描上限影响） */
  checked: number;
  /** 对平的组数 */
  matched: number;
  /** 有硬问题的组数（`QTY_MISMATCH` / `MISSING_TRANSIT` / `ORPHAN_TRANSIT` / `IDENTITY_BROKEN` / `MULTI_WAREHOUSE`） */
  mismatched: number;
  /** 整批作废的组数（`CANCELLED_ONLY`，不算不平） */
  cancelled: number;
  /** 未纳入在途账的组数（有明细、没有在途行） */
  untracked: number;
  /** 已纳入在途账的发货量合计 */
  item_qty: number | string;
  /** 在途账记的发货量合计 */
  shipped_qty: number | string;
  /** 已出合计 */
  received_qty: number | string;
  /** 在途合计 */
  in_transit_qty: number | string;
  /** 作废行发货量合计 */
  cancelled_qty: number | string;
  /** 未纳入在途账的发货量合计 */
  untracked_item_qty: number | string;
  /** 对账差额 = 已纳入发货量 −（已出 + 在途）；`0` = 整体恒等成立 */
  diff: number | string;
}

/**
 * 对账报告（逐字抄 `TransitReconcileRes`）。
 *
 * ⚠️ `rows` **缺省只给有差异的组**（`include_matched=true` 才连对平的一起给）；
 * `balanced` 只有「零硬问题 + 差额 0 + 未截断」才为真 —— 页面因此不能拿
 * `rows.length === 0` 直接说「对平」（筛选下为空、扫描被截断都不等于对平）。
 */
export interface TransitReconcile {
  /** 比对的组数 */
  checked: number;
  /** 对平的组数 */
  matched: number;
  /** 有硬问题的组数 */
  mismatched: number;
  /** 整体是否对平 */
  balanced: boolean;
  /** 是否被扫描上限截断（`true` ⇒ 结论只覆盖前 N 组，要收窄日期区间再对） */
  truncated: boolean;
  /** 扫描上限（回显，便于写进提示文案） */
  scan_limit: number;
  /** 明细（缺省只有差异组） */
  rows: TransitReconcileRow[];
  /** 汇总 */
  totals: TransitReconcileTotals;
}

/**
 * 对账入参（后端 `TransitReconcileFilter`，字段名逐字一致；**没有 `status`**）。
 *
 * `include_matched` 用 `'true' | 'false'` 字符串：后端收的是 `String`
 * （`true/false/1/0/yes/no/on/off`，其它值 400 + 66006），框架的 `bool` 只认 `true/false`。
 */
export interface TransitReconcileQuery {
  /** 发货单 id（精确 UUID） */
  shipment_id?: string;
  /** 物料 id（精确 UUID） */
  material_id?: string;
  /** 发货仓库 id（精确 UUID） */
  warehouse_id?: string;
  /** 发货日期下界（`yyyy-MM-dd`，含当天） */
  ship_date_from?: string;
  /** 发货日期上界（`yyyy-MM-dd`，含当天） */
  ship_date_to?: string;
  /** 是否连**对平**的组一起返回（缺省不发 = 后端默认 `false`） */
  include_matched?: 'true' | 'false';
}

/**
 * 在途对账（`GET /business/erp/inventory-transit/reconcile`，权限
 * `business:erp:inventory-transit:audit` —— 与明细的 `:list` **不是同一个码**）。
 *
 * 只读：后端一行写 SQL 都没有。返回「逐 `发货单 × 物料` 的三侧数量 + 差异码 + 汇总」。
 */
export function fetchTransitReconcile(query: TransitReconcileQuery = {}): Promise<TransitReconcile> {
  return get<TransitReconcile>(`${BASE}/inventory-transit/reconcile`, asParams(query));
}

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
