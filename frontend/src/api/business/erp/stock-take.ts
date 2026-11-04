/**
 * 盘点单 API 层（DC-14 后端 `backend/src/business/erp/stocktake_{dto,handler,model}.rs`）。
 *
 * # 为什么单独一个文件（不并进 `erp/index.ts`）
 *
 * 与 `stock-out.ts` 同一个理由：`index.ts` 已经是 3800+ 行的仓储域总契约，而盘点单是
 * 独立后端模块（自己的 handler / DTO / 迁移 `V0211__erp_stock_take.up.sql`）。
 * 按模块边界单独成文件，页面只 `import '@/api/business/erp/stock-take'`，不会和别人
 * 正在改的 `index.ts` 互相踩。
 *
 * # 字段名逐字抄后端（不是猜的）
 *
 * | 语义 | 真字段名 | 出处 |
 * |------|----------|------|
 * | 单号 | `take_no`（`ST-yyyyMMdd-000001`） | `stocktake_dto.rs::StockTakeRes::take_no` |
 * | 类型 | `take_type` + `take_type_label`（全盘 / 抽盘） | 后端给中文名，前端不维护第二份译名 |
 * | 状态 | `status` + `status_label` | 同上 |
 * | 账面数 | `items[].book_qty` | **建单那一刻的快照**，客户端改不了 |
 * | 差异 | `items[].diff_qty` | PG **生成列** `actual_qty − book_qty`，只读 |
 * | 差异金额 | `items[].diff_amount` / `total_diff_amount` | 过账时按账上真实差额写 |
 * | 可去状态 | `next_statuses` / `legal_edges` | **按钮只读这一份**，前端不写第二套状态机 |
 *
 * # 四个容易踩的点
 *
 * 1. **路径必须写全 `/api`**（后端注解不含前缀，漏了会命中 SPA fallback 返回 HTML）；
 * 2. **详情是扁平的**：`StockTakeDetailRes` 用 `#[serde(flatten)]` 把 `StockTakeRes`
 *    摊进顶层（与出库单同款，**没有** `header` 包一层）；
 * 3. **建单只传范围，不传账面数 / 单价 / 实盘数**：`StockTakeScopeItemReq` 只有
 *    `material_id / location_id`；账面数与成本由服务端从 `biz_inventory` 快照。
 *    类型上就没有那几个字段 —— 与其让后端默默忽略，不如在类型层面禁止；
 * 4. **全盘（FULL）必须不传 `items`**，抽盘（PARTIAL）必须传 1–2000 行；后端两种错法都 400。
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
 * 盘点类型（与 `stocktake_model::TAKE_TYPES` / `ck_biz_st_type` 逐字一致）。
 *
 * - `FULL` 全盘：按该仓**现有全部库存桶**自动快照明细（一条不少，这就是年底盘点的形态）；
 * - `PARTIAL` 抽盘：客户端逐行指定「物料 × 库位」，账面数仍由服务端快照。
 */
export type StockTakeType = 'FULL' | 'PARTIAL';

/**
 * 盘点单状态（与 `stocktake_model::STATUS_*` 逐字一致）。
 *
 * ```text
 * DRAFT ──提交审批──→ PENDING_POST ──过账──→ POSTED ──作废（必填原因）──→ CANCELLED
 *   │                     │                     └──────────作废──────────→ CANCELLED
 *   │                     └──撤回 / 驳回──→ DRAFT
 *   └──作废（必填原因）──────────────────────────────────→ CANCELLED
 * ```
 *
 * ⚠️ 与出库单的区别：**`DRAFT → POSTED` 不存在**（盘点必须先审批 + 独立过账），
 * 而 `PENDING_POST` 在盘点里是「已提交、等过账」——账一分没动。
 */
export type StockTakeStatus = 'DRAFT' | 'PENDING_POST' | 'POSTED' | 'CANCELLED';

/** 列表行（逐字抄 `StockTakeRes`） */
export interface StockTakeRow {
  /** 主键 */
  id: string;
  /** 盘点单号 `ST-yyyyMMdd-000001` */
  take_no: string;
  /** 盘点仓库 id */
  warehouse_id: string;
  /** 盘点日期 `YYYY-MM-DD`（决定会计期间） */
  take_date: string;
  /** 盘点类型 */
  take_type: StockTakeType | string;
  /** 盘点类型中文名（后端给：全盘 / 抽盘） */
  take_type_label: string;
  /** 状态 */
  status: StockTakeStatus | string;
  /** 状态中文名（后端给：草稿 / 待过账 / 已过账 / 已作废） */
  status_label: string;
  /** 账面数量合计（建单时快照） */
  total_book_qty: string;
  /** 实盘数量合计（填实盘数后才有意义） */
  total_actual_qty: string;
  /** 盘盈数量合计（> 0 部分） */
  total_gain_qty: string;
  /** 盘亏数量合计（绝对值） */
  total_loss_qty: string;
  /**
   * 盘盈金额（账上真实增量）。
   *
   * **草稿恒为 0**：金额要等过账按账上移动加权单价才算得出，
   * 所以列表 / 详情都要标「过账后才有值」。
   */
  total_gain_amount: string;
  /** 盘亏金额（账上真实减量，草稿恒为 0） */
  total_loss_amount: string;
  /**
   * 差异金额合计 = 盘盈金额 − 盘亏金额。
   *
   * **审批流按它分档**（种子流程 `< 10000` 一级审批，`≥ 10000` 两级），
   * 所以草稿阶段它恒为 0 —— 分档要等过账前那次重算。
   */
  total_diff_amount: string;
  /** 盘点差异凭证（未过账 / 零差异为 `null`） */
  voucher_id: string | null;
  /** 作废时的红字凭证（未作废 / 未过账为 `null`） */
  reverse_voucher_id: string | null;
  /** 提交审批的实例 id（草稿为 `null`） */
  approval_instance_id: string | null;
  /** 备注（盘点范围 / 组织说明） */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
  /** 制单人（用户 id） */
  create_by: string | null;
}

/** 明细行（逐字抄 `StockTakeItemRes`） */
export interface StockTakeItemLine {
  /** 主键（编辑实盘数时当 `item_id` 提交） */
  id: string;
  /** 行号（1 起） */
  line_no: number;
  /** 物料 id */
  material_id: string;
  /** 物料编码（后端详情补的展示名，可能为 `null` = 物料已删） */
  material_code: string | null;
  /** 物料名称 */
  material_name: string | null;
  /** 计量单位 id */
  unit_id: string;
  /** 库位账行（`biz_inventory_location.id`，`null` = 未分配库位桶） */
  location_id: string | null;
  /** 账面数（建单时从 `biz_inventory` 快照，**只读**） */
  book_qty: string;
  /** 实盘数（草稿里可改） */
  actual_qty: string;
  /** 差异 = 实盘 − 账面（PG 生成列，**只读**；负数 = 盘亏，正数 = 盘盈） */
  diff_qty: string;
  /** 单价快照（过账时按账上均价写，草稿为 `0`） */
  unit_cost: string;
  /** 差异金额（过账时写，草稿为 `0`） */
  diff_amount: string;
  /** 行备注 */
  remark: string | null;
}

/**
 * 详情（`StockTakeDetailRes`：单头 `#[serde(flatten)]` 摊平 + 明细 + 动作）。
 *
 * ⚠️ **没有 `header` 包一层** —— 直接 `detail.take_no`。
 */
export interface StockTakeDetail extends StockTakeRow {
  /** 明细 */
  items: StockTakeItemLine[];
  /** 过账时间（RFC3339） */
  posted_at: string | null;
  /** 过账人（审计：谁过的账，与制单人 / 审批人可分） */
  posted_by: string | null;
  /** 作废时间（RFC3339） */
  cancelled_at: string | null;
  /** 作废原因（已过账作废必填） */
  cancel_reason: string | null;
  /**
   * 当前状态能去哪些状态（后端算好下发）。
   *
   * **动作按钮只由它驱动**：前端不写第二份状态机，后端加了边 / 收紧了边，
   * 页面自动跟着变（`DRAFT → POSTED` 这条在盘点上**本来就没有**）。
   */
  next_statuses: string[];
  /** 写死的边表（`[(from, to)]`，与 `next_statuses` 同源，可供调试 / 单测对照） */
  legal_edges: Array<[string, string]>;
}

/** 抽盘范围的一行（**只给范围**：账面数由服务端快照） */
export interface StockTakeScopeItemReq {
  /** 物料 id（必填） */
  material_id: string;
  /** 库位账行（`biz_inventory_location.id`，可空 = 未分配桶） */
  location_id?: string | null;
}

/** 建单请求体（`POST /erp/stock-take`） */
export interface CreateStockTakeReq {
  /** 盘点仓库（必填） */
  warehouse_id: string;
  /** 盘点日期 `YYYY-MM-DD`（缺省今天 CST） */
  take_date: string;
  /** 盘点类型（`FULL` 全盘 / `PARTIAL` 抽盘） */
  take_type: StockTakeType;
  /** 备注（≤ 500 字） */
  remark?: string | null;
  /** 抽盘范围（`FULL` **必须为空数组**；`PARTIAL` 1–2000 行） */
  items: StockTakeScopeItemReq[];
}

/** 一行实盘数（改单用） */
export interface StockTakeActualReq {
  /** 明细 id（建单时返回的那个 `items[].id`） */
  item_id: string;
  /** 实盘数（≥ 0） */
  actual_qty: string;
}

/**
 * 改单请求体（`PUT /erp/stock-take/{id}`，**仅 DRAFT**）。
 *
 * 只能改「日期 / 备注 / 逐行实盘数」——**范围（哪些物料）与账面数都改不了**，
 * 否则「自己填账面数」的游戏又回来了。要换范围就作废重开一张。
 */
export interface UpdateStockTakeReq {
  /** 盘点日期 `YYYY-MM-DD` */
  take_date?: string;
  /** 备注 */
  remark?: string | null;
  /** 逐行实盘数 */
  lines: StockTakeActualReq[];
}

/**
 * 状态流转请求体（`PATCH /erp/stock-take/{id}/status`）。
 *
 * - `PENDING_POST`：提交审批（起 `stock_take_post` 流程；未配流程 → 409 `66033`）；
 * - `POSTED`：过账（三本账 +/- + 盘点差异凭证；**只有 PENDING_POST 能过**）；
 * - `DRAFT`：从待过账撤回（终止在跑的审批）；
 * - `CANCELLED`：作废，**已过账时 `reason` 必填**（进状态日志与红字凭证摘要）。
 *
 * `skip_approval` / `skip_reason` 是 `SkipApprovalReq`（`#[serde(flatten)]`）：
 * 只有「没配已发布流程 + 持 `business:erp:skip-approval` + 理由 ≥ 10 字」才放行。
 */
export interface StockTakeStatusReq {
  /** 目标状态 */
  status: StockTakeStatus;
  /** 原因（已过账作废必填） */
  reason?: string | null;
  /** 是否显式跳过审批（缺省不传 = 没请求） */
  skip_approval?: boolean;
  /** 跳过审批的理由（≥ 10 字，会写进 `biz_status_log`） */
  skip_reason?: string | null;
}

/** 盘点单分页（`GET /erp/stock-take/page`，`list` 元素是扁平 `StockTakeRes`） */
export function fetchStockTakePage(query: {
  /** 单号关键字（模糊） */
  keyword?: string;
  status?: StockTakeStatus | string;
  take_type?: StockTakeType | string;
  warehouse_id?: string;
  page?: number;
  page_size?: number;
}): Promise<PageData<StockTakeRow>> {
  return get<PageData<StockTakeRow>>(`${BASE}/stock-take/page`, asParams(query));
}

/** 盘点单详情（`GET /erp/stock-take/{id}`，**扁平**：没有 `header` 包一层） */
export function fetchStockTakeDetail(id: string): Promise<StockTakeDetail> {
  return get<StockTakeDetail>(`${BASE}/stock-take/${id}`);
}

/**
 * 建单（`POST /erp/stock-take`）。
 *
 * 返回的是**完整详情**（含服务端快照出来的账面数），前端不用再发一次 GET。
 */
export function createStockTake(payload: CreateStockTakeReq): Promise<StockTakeDetail> {
  return post<StockTakeDetail>(`${BASE}/stock-take`, payload);
}

/** 改单（`PUT /erp/stock-take/{id}`，**仅 DRAFT**，只改日期 / 备注 / 实盘数） */
export function updateStockTake(id: string, payload: UpdateStockTakeReq): Promise<StockTakeDetail> {
  return put<StockTakeDetail>(`${BASE}/stock-take/${id}`, payload);
}

/** 删除（`DELETE /erp/stock-take/{id}`，**仅 DRAFT**，软删） */
export function deleteStockTake(id: string): Promise<void> {
  return del<void>(`${BASE}/stock-take/${id}`);
}

/**
 * 状态流转（`PATCH /erp/stock-take/{id}/status`）。
 *
 * 失败口径（页面要原样展示报文，不要只写「操作失败」）：
 * - `409 + 66033` 没有已发布审批流程：报文点名 `stock_take_post`，并给出两条出路；
 * - `403 + 10003` 请求跳过但没有 `business:erp:skip-approval`；
 * - `400 + 10001` 跳过理由 < 10 字 / 已过账作废没写原因；
 * - `409 + 66001` 状态边不合法（报文列当前状态与合法路径）。
 */
export function updateStockTakeStatus(
  id: string,
  payload: StockTakeStatusReq,
): Promise<StockTakeDetail> {
  return patch<StockTakeDetail>(`${BASE}/stock-take/${id}/status`, payload);
}
