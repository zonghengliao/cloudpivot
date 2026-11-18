/**
 * 批次成本页类型与词表（M5-07 · 分批法「批次成本明细」）。
 *
 * # 与后端的一一对应（8 条接口 / 4 个 DTO 文件，字段逐字抄，不自己取名）
 *
 * | DTO 文件 | 出参 |
 * |---|---|
 * | `cost_collect_sheet_dto.rs` | `SheetRes` / `SheetDetailRes`（M5-02 归集表 + 明细） |
 * | `cost_batch_collect_dto.rs` | `BatchSourcesRes` / `BatchCollectRes`（M5-03 三路来源 + 归集） |
 * | `cost_batch_finish_dto.rs` | `BatchFinishPreviewRes` / `BatchFinishRes`（M5-04 结转试算 + 结转） |
 * | `cost_batch_wip_dto.rs` | `WipBalanceRes` / `WipLedgerRes`（M5-05 在制余额 + 台账） |
 *
 * # 两套金额形状，别混（踩过就知道疼）
 *
 * * 归集表 / 明细：`{ mat, labor, oh, total }`（`SheetAmounts`）
 * * 结转 / 在制：`{ material, labor, overhead, total }`（`AmountsRes` / `WipAmountsRes`）
 *
 * 同一个概念两套字段名是后端既成契约（`SheetAmounts` 早于 `AmountsRes`），
 * 前端**不做别名抹平**：抹平意味着一处改名字要同步改 4 个 DTO 的映射层，
 * 而映射层正是最容易把「材料写成制造费用」的地方。页面各按各的形状读。
 *
 * # 金额一律是字符串
 *
 * 后端 `rust_decimal::Decimal` 序列化成 `"1200.0000"`。**不要 `+`**，
 * 一律过 `views/business/erp/shared/money` 的 `toCents` / `formatMoney`。
 */

// ==================== 权限码（复用既有码，不新增） ====================

/**
 * 本页两个权限码（都是**复用**，不是新开）：
 *
 * * `read` = `business:erp:cost-object:list` —— 四条只读接口（归集表 / 明细 / 来源 /
 *   结转试算 / 在制余额 / 台账）全挂这个码，理由见 `cost_collect_sheet_dto.rs::PERM_LIST`：
 *   本页每一行都是一个成本对象，每一列都能用同一个码从成本对象页读出来，不多露一分；
 * * `write` = `business:erp:cost-period:calc` —— 归集与结转的语义就是「算成本」，
 *   `calc` 的第 1/2/3 段做的正是这两件事，收窄到单个批次不改变口径。
 *
 * 新开码必然要种 `sys_menu`，而种了就要有真路由对应，否则
 * `permission_audit_tests::n14` 会判「菜单码 0 落地」（死码）。所以这里**不许**加码。
 */
export const COST_BATCH_PERMISSIONS = {
  /** 读：归集表 / 明细 / 三路来源 / 结转试算 / 在制余额 / 台账 */
  read: 'business:erp:cost-object:list',
  /** 写：归集本期、完工结转 */
  write: 'business:erp:cost-period:calc',
} as const;

// ==================== 批次状态（与 SheetFilter 值域同源） ====================

/** 状态筛选值域（`cost_collect_sheet_dto::STATUSES`，顺序即下拉顺序）。 */
export type CostBatchStatus = 'ALL' | 'OPEN' | 'CLOSED' | 'CANCELLED';

/** 全部状态值（顺序即界面顺序：先「全部」，再按生命周期）。 */
export const COST_BATCH_STATUSES: readonly CostBatchStatus[] = ['ALL', 'OPEN', 'CLOSED', 'CANCELLED'];

/** 状态中文（`ALL` 只出现在筛选里；行上的 status 只有后三个取值）。 */
export const COST_BATCH_STATUS_LABELS: Record<CostBatchStatus, string> = {
  ALL: '全部状态',
  OPEN: '在产',
  CLOSED: '已关批',
  CANCELLED: '已作废',
};

/** 默认状态筛选 = 不限（后端 `DEFAULT_STATUS`，理由见该常量注释：月内关批是常态）。 */
export const COST_BATCH_DEFAULT_STATUS: CostBatchStatus = 'ALL';

// ==================== 排序白名单（两个列表各一套） ====================

/** 归集表排序（`cost_collect_sheet_dto::SORTS`）。 */
export type SheetSort = 'total' | 'object_no' | 'order_no' | 'start_date';

/** 归集表排序值域与中文（默认 = 合计倒序：打开就看「本月最贵的批」）。 */
export const SHEET_SORTS: ReadonlyArray<{ value: SheetSort; label: string }> = [
  { value: 'total', label: '合计金额' },
  { value: 'object_no', label: '批次号' },
  { value: 'order_no', label: '生产订单号' },
  { value: 'start_date', label: '开工日期' },
];

/** 归集表默认排序。 */
export const SHEET_DEFAULT_SORT: SheetSort = 'total';

/** 在制余额排序（`cost_batch_wip_dto::SORTS`，默认 `balance`：哪批押着最多钱）。 */
export type WipSort = 'balance' | 'object_no' | 'order_no' | 'start_date';

/** 在制余额排序值域与中文。 */
export const WIP_SORTS: ReadonlyArray<{ value: WipSort; label: string }> = [
  { value: 'balance', label: '在制余额' },
  { value: 'object_no', label: '批次号' },
  { value: 'order_no', label: '生产订单号' },
  { value: 'start_date', label: '开工日期' },
];

/** 在制余额默认排序。 */
export const WIP_DEFAULT_SORT: WipSort = 'balance';

// ==================== 三要素 ====================

/** 要素值域（`MATERIAL` / `LABOR` / `OVERHEAD`，与归集行 `element` 同源）。 */
export const COST_BATCH_ELEMENTS = ['MATERIAL', 'LABOR', 'OVERHEAD'] as const;

/** 要素类型。 */
export type CostBatchElement = (typeof COST_BATCH_ELEMENTS)[number];

/** 要素中文（后端也带 `element_label`，这里只在兜底 / 标签页标题用）。 */
export const COST_BATCH_ELEMENT_LABELS: Record<CostBatchElement, string> = {
  MATERIAL: '直接材料',
  LABOR: '直接人工',
  OVERHEAD: '制造费用',
};

// ==================== 来源行三态（M5-03） ====================

/** 一行来源的状态（`cost_batch_collect_dto::STATE_*`）。 */
export type SourceState = 'COLLECTED' | 'PENDING' | 'UNCOLLECTABLE';

/** 状态中文（与后端 `state_label` 逐字一致 —— 出参里也带一份，这里只做兜底）。 */
export const SOURCE_STATE_LABELS: Record<SourceState, string> = {
  COLLECTED: '已归集',
  PENDING: '待归集',
  UNCOLLECTABLE: '不可归集',
};

// ==================== 归集表 / 明细（M5-02） ====================

/** 三要素金额 + 合计（合计由三项相加，后端保证 `total = mat + labor + oh`）。 */
export interface SheetAmounts {
  /** 直接材料。 */
  mat: string;
  /** 直接人工。 */
  labor: string;
  /** 制造费用。 */
  oh: string;
  /** 合计。 */
  total: string;
}

/**
 * 整张表的合计（**不是当前页的合计**）。
 *
 * ⚠️ 形状是**扁的**：后端 `SheetTotalsRes` 对 `amounts` 用了 `#[serde(flatten)]`，
 * 出 JSON 时四项直接摊在顶层 —— 真跑 HTTP 抓到的形状就是
 * `{"mat":"0.0000","labor":"0.0000","oh":"0.0000","total":"0.0000","line_count":0,"batch_count":0}`
 * （见 `docs/evidence/2026-11-19-t_b26f00e5-cost-batch-page/03_shape_dump.out`）。
 * 想成嵌套 `{amounts:{...}}` 会让页脚合计永远是 0 且编译期不报错 —— 这类错只有真跑才抓得到。
 */
export interface SheetTotals extends SheetAmounts {
  /** 归集行数合计。 */
  line_count: number;
  /** 有归集行（`line_count > 0`）的批次数。 */
  batch_count: number;
}

/** 归集表一行（一个批次）。 */
export interface SheetRow {
  /** 成本对象 id（详情 / 来源 / 结转 / 台账都用它）。 */
  object_id: string;
  /** 批次号 `CB-yyyyMM-000001`。 */
  object_no: string;
  /** 对象类型（一期恒 `BATCH`）。 */
  object_type: string;
  /** 生产订单号（可空：备货生产没有订单）。 */
  source_doc_no: string | null;
  /** 产出批号。 */
  lot_no: string | null;
  /** 产出品 id。 */
  product_id: string;
  /** 产出品编码。 */
  product_code: string | null;
  /** 产出品名称。 */
  product_name: string | null;
  /** 车间部门 id。 */
  workshop_dept_id: string;
  /** 车间部门名。 */
  workshop_dept_name: string | null;
  /** 计划数量。 */
  plan_qty: string;
  /** 累计完工数量。 */
  completed_qty: string;
  /** 在制数量。 */
  wip_qty: string;
  /** 完工率（百分数串，如 `66.67`）。 */
  completion_pct: string;
  /** 状态（`OPEN` / `CLOSED` / `CANCELLED`）。 */
  status: string;
  /** 开工日期 `YYYY-MM-DD`。 */
  start_date: string;
  /** 计划完工日期。 */
  due_date: string | null;
  /**
   * 本期金额·直接材料（后端 `#[serde(flatten)]`，**扁的**，见 {@link SheetTotals}）。
   */
  mat: string;
  /** 本期金额·直接人工。 */
  labor: string;
  /** 本期金额·制造费用。 */
  oh: string;
  /** 本期金额·合计（后端算好；页面仍按三项相加再算一次，不等就是后端 bug）。 */
  total: string;
  /** 本期归集行数（0 = 本月既没领料也没报工，是缺口不是噪声）。 */
  line_count: number;
  /** 最后一笔归集日期。 */
  last_occur_date: string | null;
}

/** 归集表出参（字段名与后端 `SheetRes` 逐字一致，可直接喂 `ListPage` 的 fetcher）。 */
export interface SheetPage {
  /** 查询期间 `yyyy-MM`。 */
  period_code: string;
  /** 整张表合计。 */
  totals: SheetTotals;
  /** 当前页行。 */
  list: SheetRow[];
  /** 总行数。 */
  total: number;
  /** 当前页。 */
  page: number;
  /** 每页条数。 */
  page_size: number;
}

/** 批次抬头（明细 / 来源 / 结转 / 台账四处的 `batch` 是同一个形状）。 */
export interface SheetBatch {
  /** 成本对象 id。 */
  object_id: string;
  /** 批次号。 */
  object_no: string;
  /** 对象类型。 */
  object_type: string;
  /** 生产订单号。 */
  source_doc_no: string | null;
  /** 产出批号。 */
  lot_no: string | null;
  /** 产出品 id。 */
  product_id: string;
  /** 产出品编码。 */
  product_code: string | null;
  /** 产出品名称。 */
  product_name: string | null;
  /** 车间部门 id。 */
  workshop_dept_id: string;
  /** 车间部门名。 */
  workshop_dept_name: string | null;
  /** 计划数量。 */
  plan_qty: string;
  /** 累计完工数量。 */
  completed_qty: string;
  /** 在制数量。 */
  wip_qty: string;
  /** 完工率。 */
  completion_pct: string;
  /** 状态。 */
  status: string;
  /** 开工日期。 */
  start_date: string;
  /** 计划完工日期。 */
  due_date: string | null;
}

/** 一条归集明细（`biz_cost_collect` 一行）。 */
export interface SheetLine {
  /** 归集行 id。 */
  id: string;
  /** 要素（`MATERIAL` / `LABOR` / `OVERHEAD`）。 */
  element: string;
  /** 要素中文（后端直出）。 */
  element_label: string;
  /** 来源类型（`STOCK_OUT` / `LABOR_TICKET` / `OVERHEAD_TICKET`）。 */
  source_type: string;
  /** 来源类型中文。 */
  source_type_label: string;
  /** 来源单 id。 */
  source_id: string | null;
  /** 来源单号。 */
  source_no: string | null;
  /** 来源行号。 */
  line_no: number;
  /** 发生日期。 */
  occur_date: string;
  /** 物料 id。 */
  material_id: string | null;
  /** 物料编码。 */
  material_code: string | null;
  /** 物料名称。 */
  material_name: string | null;
  /** 数量。 */
  qty: string | null;
  /** 单价。 */
  unit_price: string | null;
  /** 金额。 */
  amount: string;
  /** 部门名。 */
  dept_name: string | null;
  /** 备注。 */
  remark: string | null;
}

/** 批次归集明细出参（`SheetDetailRes`）。 */
export interface SheetDetail {
  /** 期间。 */
  period_code: string;
  /** 批次抬头。 */
  batch: SheetBatch;
  /** 合计（由明细行相加，与归集表该行必然相等）。 */
  totals: SheetAmounts;
  /** 逐笔归集行。 */
  lines: SheetLine[];
}

// ==================== 三路来源 / 归集（M5-03） ====================

/** 期间成本批次的现状（只带闸门需要的两列 + 派生标签）。 */
export interface PeriodBatch {
  /** 批次号 `CS-yyyyMM-0001`。 */
  batch_no: string;
  /** 状态（`DRAFT` / `SUBMITTED` / `APPROVED` / `SETTLED` / `CLOSED` …）。 */
  status: string;
  /** 状态中文。 */
  status_label: string;
  /** 是否已冻结（`SUBMITTED` 之后四态）。 */
  frozen: boolean;
}

/** 一行来源候选。 */
export interface SourceLine {
  /** 要素。 */
  element: string;
  /** 来源类型。 */
  source_type: string;
  /** 来源单 id。 */
  source_id: string;
  /** 来源单号。 */
  source_no: string | null;
  /** 发生日期。 */
  occur_date: string;
  /** 展示标签（后端拼好，如「领料出库」/「工时单」）。 */
  label: string;
  /** 数量。 */
  qty: string | null;
  /** 单价。 */
  unit_price: string | null;
  /** 工时。 */
  hours: string | null;
  /** 费率。 */
  rate: string | null;
  /** 物料 id。 */
  material_id: string | null;
  /** 物料编码。 */
  material_code: string | null;
  /** 物料名称。 */
  material_name: string | null;
  /** 科目编码（手工费用）。 */
  account_code: string | null;
  /** 部门名。 */
  dept_name: string | null;
  /** 三态。 */
  state: SourceState;
  /** 三态中文（后端直出）。 */
  state_label: string;
  /** 不可归集的原因（`UNCOLLECTABLE` 才有）。 */
  reason: string | null;
  /** 金额。 */
  amount: string;
}

/** 一路要素的来源汇总 + 候选行。 */
export interface ElementSources {
  /** 要素。 */
  element: string;
  /** 要素中文。 */
  element_label: string;
  /** 候选行。 */
  rows: SourceLine[];
  /** 候选行数。 */
  rows_total: number;
  /** 候选金额合计。 */
  amount_total: string;
  /** 已归集行数。 */
  collected_rows: number;
  /** 已归集金额。 */
  collected_amount: string;
  /** 本次将写行数。 */
  pending_rows: number;
  /** 本次将写金额。 */
  pending_amount: string;
  /** 过期将被清掉的行数（来源变了：金额改小 / 对象改挂 / 单据作废）。 */
  purged_rows: number;
  /** 过期将被清掉的金额。 */
  purged_amount: string;
  /** 不可归集行数。 */
  uncollected_rows: number;
  /** 不可归集金额。 */
  uncollected_amount: string;
  /** 归集后该要素的累计金额。 */
  after_amount: string;
}

/** 三路来源预览出参（`BatchSourcesRes`）。 */
export interface BatchSources {
  /** 期间。 */
  period_code: string;
  /** 批次抬头。 */
  batch: SheetBatch;
  /** 期间成本批次（`None` = 本期还没建批次）。 */
  period_batch: PeriodBatch | null;
  /** 现在能不能归集。 */
  collectable: boolean;
  /** 不能归集的原因。 */
  block_reason: string | null;
  /** 怎么修。 */
  block_fix: string | null;
  /** 归集后是否要让计算单跟上（本期已有 `DRAFT` 批次）。 */
  recalc_required: boolean;
  /** 材料。 */
  material: ElementSources;
  /** 人工。 */
  labor: ElementSources;
  /** 制造费用。 */
  overhead: ElementSources;
  /** 归集前合计。 */
  totals: SheetAmounts;
  /** 归集后合计。 */
  after_totals: SheetAmounts;
}

/** 一路要素的归集结果。 */
export interface ElementCollect {
  /** 要素。 */
  element: string;
  /** 要素中文。 */
  element_label: string;
  /** 扫描到的候选行。 */
  scanned: number;
  /** 新写入行数。 */
  inserted: number;
  /** 幂等命中的行数（已在，跳过）。 */
  skipped_existing: number;
  /** 过期清掉的行数。 */
  purged: number;
  /** 新写入金额。 */
  inserted_amount: string;
  /** 清掉的金额。 */
  purged_amount: string;
  /** 归集后已归集行数。 */
  collected_rows: number;
  /** 归集后已归集金额。 */
  collected_amount: string;
  /** 仍不可归集行数。 */
  uncollected_rows: number;
  /** 仍不可归集金额。 */
  uncollected_amount: string;
}

/** 归集执行出参（`BatchCollectRes`）。 */
export interface BatchCollect {
  /** 期间。 */
  period_code: string;
  /** 批次抬头。 */
  batch: SheetBatch;
  /** 期间成本批次。 */
  period_batch: PeriodBatch | null;
  /** 是否需要重跑 calc。 */
  recalc_required: boolean;
  /** 材料结果。 */
  material: ElementCollect;
  /** 人工结果。 */
  labor: ElementCollect;
  /** 制造费用结果。 */
  overhead: ElementCollect;
  /** 归集后合计。 */
  totals: SheetAmounts;
  /** 后端拼好的一句话回执（直接展示）。 */
  message: string;
}

// ==================== 完工结转（M5-04） ====================

/** 三要素金额 + 合计（结转 / 在制口径的字段名：`material` / `labor` / `overhead`）。 */
export interface AmountsRes {
  /** 直接材料。 */
  material: string;
  /** 直接人工。 */
  labor: string;
  /** 制造费用。 */
  overhead: string;
  /** 合计。 */
  total: string;
}

/** 一条结转闸门（拦下本次结转的原因 + 怎么修）。 */
export interface FinishBlocker {
  /** 机器码（`WIP_NOT_ZERO` / `UNCOLLECTED_STOCK_OUT` …）。 */
  code: string;
  /** 为什么不能转。 */
  message: string;
  /** 怎么修。 */
  fix: string;
}

/** 一张历史结转单。 */
export interface FinishHistory {
  /** 结转单 id。 */
  id: string;
  /** 同批第几次结转。 */
  transfer_seq: number;
  /** 结转期间。 */
  period_code: string;
  /** 完工数量（结转口径）。 */
  finished_qty: string;
  /** 本次单位成本。 */
  unit_cost: string;
  /** 三要素金额。 */
  amounts: AmountsRes;
  /** 创建时间。 */
  created_at: string;
  /** 创建人姓名（快照）。 */
  created_by_name: string | null;
  /** 备注。 */
  remark: string | null;
}

/** 结转试算出参（`BatchFinishPreviewRes`）。 */
export interface BatchFinishPreview {
  /** 批次抬头。 */
  batch: SheetBatch;
  /** 期间。 */
  period_code: string;
  /** 期间成本批次。 */
  period_batch: PeriodBatch | null;
  /** 累计归集。 */
  collected: AmountsRes;
  /** 已结转。 */
  transferred: AmountsRes;
  /** 本次待结转（= 累计归集 − 已结转，逐要素夹零）。 */
  pending: AmountsRes;
  /** 完工数量。 */
  finished_qty: string;
  /** 本次单位成本。 */
  unit_cost: string;
  /** 结转后累计单位成本。 */
  cumulative_unit_cost: string;
  /** 本次是否有钱可转（差为 0 ⇒ 幂等空转）。 */
  transferable: boolean;
  /** 本次结转序号。 */
  transfer_no: number;
  /** 是否已有已结算计算单（账务已经走过）。 */
  settled_in_books: boolean;
  /** 历史结转单。 */
  history: FinishHistory[];
  /** 闸门（空 = 可结转）。 */
  blockers: FinishBlocker[];
  /** 提示（不拦人，但要说清口径）。 */
  warnings: string[];
}

/** 结转执行出参（`BatchFinishRes`）。 */
export interface BatchFinish {
  /** 是否真写了新单（`false` = 幂等空转，钱已经转完）。 */
  inserted: boolean;
  /** 回执。 */
  message: string;
  /** 结转单 id（空转时为 `null`）。 */
  finish_id: string | null;
  /** 本次结转金额。 */
  transfer: AmountsRes;
  /** 结转后的剩余（应恒为 0）。 */
  remaining: AmountsRes;
  /** 结转后的完整试算（页面直接用它刷新，不额外再查一次）。 */
  preview: BatchFinishPreview;
}

// ==================== 在制余额 / 台账（M5-05） ====================

/** 在制口径的三要素金额（字段名与 `SheetAmounts` 不同，见模块头）。 */
export interface WipAmounts {
  /** 直接材料。 */
  material: string;
  /** 直接人工。 */
  labor: string;
  /** 制造费用。 */
  overhead: string;
  /** 合计。 */
  total: string;
}

/** 在制余额一行（一个批次）。 */
export interface WipRow {
  /** 成本对象 id。 */
  object_id: string;
  /** 批次号。 */
  object_no: string;
  /** 对象类型。 */
  object_type: string;
  /** 生产订单号。 */
  source_doc_no: string | null;
  /** 产出批号。 */
  lot_no: string | null;
  /** 产出品 id。 */
  product_id: string;
  /** 产出品编码。 */
  product_code: string | null;
  /** 产出品名称。 */
  product_name: string | null;
  /** 车间 id。 */
  workshop_dept_id: string;
  /** 车间名。 */
  workshop_dept_name: string | null;
  /** 计划数量。 */
  plan_qty: string;
  /** 累计完工数量。 */
  completed_qty: string;
  /** 在制数量。 */
  wip_qty: string;
  /** 完工率。 */
  completion_pct: string;
  /** 状态。 */
  status: string;
  /** 开工日期。 */
  start_date: string;
  /** 计划完工日期。 */
  due_date: string | null;
  /** 累计归集（截止期间）。 */
  collected: WipAmounts;
  /** 累计已结转（截止期间）。 */
  transferred: WipAmounts;
  /** 在制余额（不夹零：负数是异常，要查）。 */
  balance: WipAmounts;
  /** 本期投入。 */
  period_in: WipAmounts;
  /** 本期转出。 */
  period_out: WipAmounts;
  /** 累计归集行数。 */
  collect_lines: number;
  /** 累计结转次数。 */
  transfer_times: number;
  /** 首笔归集期间。 */
  first_period: string | null;
  /** 最后一笔发生期间。 */
  last_period: string | null;
  /** 最后一次结转时间。 */
  last_transfer_at: string | null;
  /** 是否有余额（三要素任一非 0）。 */
  has_balance: boolean;
  /** 警告码（`NEGATIVE_BALANCE` / `WIP_WITHOUT_COST` …）。 */
  warnings: string[];
}

/** 在制余额整表合计。 */
export interface WipTotals {
  /** 累计归集。 */
  collected: WipAmounts;
  /** 累计已结转。 */
  transferred: WipAmounts;
  /** 在制余额。 */
  balance: WipAmounts;
  /** 本期投入。 */
  period_in: WipAmounts;
  /** 本期转出。 */
  period_out: WipAmounts;
  /** 批次数。 */
  batch_count: number;
  /** 有余额的批次数。 */
  wip_batch_count: number;
  /** 余额为负的批次数（>0 说明账要查）。 */
  negative_batch_count: number;
}

/** 在制余额出参（`WipBalanceRes`）。 */
export interface WipBalancePage {
  /** 期间。 */
  period_code: string;
  /** 整表合计。 */
  totals: WipTotals;
  /** 当前页行。 */
  list: WipRow[];
  /** 总行数。 */
  total: number;
  /** 当前页。 */
  page: number;
  /** 每页条数。 */
  page_size: number;
}

/** 台账的一个期间（滚动式：期初 + 投入 − 转出 = 期末）。 */
export interface LedgerPeriod {
  /** 期间。 */
  period_code: string;
  /** 期初。 */
  opening: WipAmounts;
  /** 本期投入（归集）。 */
  inflow: WipAmounts;
  /** 本期转出（结转）。 */
  outflow: WipAmounts;
  /** 期末。 */
  closing: WipAmounts;
  /** 本期归集行数。 */
  collect_lines: number;
  /** 本期结转次数。 */
  transfer_times: number;
}

/** 台账汇总行（累计式）。 */
export interface WipSummary {
  /** 累计归集。 */
  collected: WipAmounts;
  /** 累计已结转。 */
  transferred: WipAmounts;
  /** 在制余额。 */
  balance: WipAmounts;
  /** 累计归集行数。 */
  collect_lines: number;
  /** 累计结转次数。 */
  transfer_times: number;
}

/** 一条警告（只读接口不拦人，但要说清）。 */
export interface WipWarning {
  /** 机器码。 */
  code: string;
  /** 级别：`WARN` / `INFO`。 */
  level: string;
  /** 哪里不对。 */
  message: string;
  /** 怎么办。 */
  hint: string;
}

/** 截止期间之后还有发生（本表是「截至 P」的快照）。 */
export interface LaterActivity {
  /** 之后累计归集。 */
  collected_total: string;
  /** 之后累计结转。 */
  transferred_total: string;
}

/** 已结算计算单（期间结算口径）。 */
export interface Settled {
  /** 结算期间。 */
  period_code: string;
  /** 完工转出。 */
  finished_cost: string;
  /** 期末在制（约当产量口径）。 */
  wip_end_cost: string;
}

/** 在制台账出参（`WipLedgerRes`）。 */
export interface WipLedger {
  /** 截止期间。 */
  period_code: string;
  /** 批次抬头。 */
  batch: SheetBatch;
  /** 期初余额（截断时含被折进的最早若干期净流量）。 */
  opening: WipAmounts;
  /** 汇总（累计式）。 */
  summary: WipSummary;
  /** 逐期回放。 */
  periods: LedgerPeriod[];
  /** 是否因期间过多截断了最早的若干期。 */
  truncated: boolean;
  /** 截止期间之后的活动。 */
  after_period: LaterActivity | null;
  /** 已结算计算单。 */
  settled: Settled | null;
  /** 警告。 */
  warnings: WipWarning[];
  /** 口径说明（后端写好的整句，前端直接展示）。 */
  notes: string[];
}

// ==================== 查询入参 ====================

/** 归集表查询（`GET /business/erp/cost-collect-sheet`）。 */
export interface CollectSheetQuery {
  /** 会计期间 `yyyy-MM`（**必填**：归集额是「期间 × 批次」的量）。 */
  period_code: string;
  /** 关键字（批次号 / 生产订单号 / 产出批号 / 产出品，模糊）。 */
  keyword?: string;
  /** 车间部门 id。 */
  workshop_dept_id?: string;
  /** 状态（缺省 / `ALL` = 不限）。 */
  status?: CostBatchStatus;
  /** 排序键（缺省 `total`）。 */
  sort?: SheetSort;
  /** 页码。 */
  page?: number;
  /** 每页条数。 */
  page_size?: number;
}

/** 在制余额查询（`GET /business/erp/cost-collect-sheet/wip-balance`）。 */
export interface WipBalanceQuery {
  /** 会计期间 `yyyy-MM`（必填，同上）。 */
  period_code: string;
  /** 关键字。 */
  keyword?: string;
  /** 车间部门 id。 */
  workshop_dept_id?: string;
  /** 状态。 */
  status?: CostBatchStatus;
  /** 只看有余额的批（`true` 时后端加 `balance <> 0` 过滤）。 */
  only_wip?: boolean;
  /** 排序键（缺省 `balance`）。 */
  sort?: WipSort;
  /** 页码。 */
  page?: number;
  /** 每页条数。 */
  page_size?: number;
}
