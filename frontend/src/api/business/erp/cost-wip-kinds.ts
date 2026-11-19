/**
 * 在制品页类型与词表（M5-25【M5·成本】在制品：前端页）。
 *
 * 一页两个页签，各消费一条后端接口 —— **字段逐字抄后端 DTO，不自己取名**：
 *
 * | 页签 | 接口 | 后端文件 | 本文件的类型 |
 * |---|---|---|---|
 * | 完工率录入 | `GET/PUT /business/erp/cost-object/{id}/operations` | `cost_wip_pct_dto.rs`（M5-21） | {@link WipOperationRes} / {@link WipOperationSaveReq} |
 * | 在制余额 | `GET /business/erp/cost-wip-balance` | `cost_wip_balance_dto.rs`（M5-23） | {@link WipBalanceRes} |
 *
 * 两个接口都**复用** `business:erp:cost-object:list`（读）/ `:update`（写），本卡零新增权限码
 * （ERP 全域尚未做「页面码 / 接口码解耦」，PERM-01 只在 HR 试点 —— 新开 `cost-wip:*`
 * 会立刻变成没有菜单行承载的死码，`permission_audit_tests::n14` 判红）。
 *
 * # 数值一律是字符串（前端只展示，不重算）
 *
 * 后端 `rust_decimal::Decimal` 出 JSON 是字符串：完工率 `"50.00"`（2 位）、
 * 权重 `"2.0000"`（4 位）、金额 `"350.0000"`（4 位）、数量 `"40.0000"`（4 位）。
 * 前端**不做任何业务算术**：综合完工率、贡献、占比、期初还原、余额滚动全在后端
 * （`cost_wip_pct_dto::rollup_of` / `cost_wip_balance_dto::balance_of`，两侧都有单测钉着）。
 *
 * # 三种 `null` 的含义（别当 0 处理）
 *
 * | 字段 | `null` 的含义 |
 * |---|---|
 * | `rollup` | 该批还没有任何工序（没有工序就没有「加权」这回事） |
 * | `wip_total` / `beg_total` / `cur_total` / `finished_total` | 这一行的余额算不出来（`carry.state = blocked`，原因在 `issues[]`） |
 * | `elements[].*` | 已结算行的试算与账面不一致（分要素拆出来加不齐账面余额，见 `ledger.agrees`） |
 * | `carry.to_beg_qty` / `to_beg_cost` | 下一期还没有计算单（`pending`），没有「实际取走」的数 |
 *
 * 把这四种 `null` 画成 `0` 是这张表最容易被误读的地方：`0` 会被读成「没有余额」，
 * 而真相可能是「算不出来」或「还没入账」。
 */

// ==================== 权限码（复用成本对象，零新增） ====================

/**
 * 本页权限码。
 *
 * 与后端 `cost_wip_pct_dto::PERM_READ / PERM_WRITE`、`cost_wip_balance_dto::PERM_LIST`
 * **逐字相同**（后端单测 `permission_codes_match_the_cost_object_handler` 钉住不漂移）：
 *
 * - 读 = `business:erp:cost-object:list`：每行都是某个成本对象的一列数，看得到成本对象的人
 *   本来就该看到「这 40 件在制折几件完工」「这笔余额结没结转」；
 * - 写 = `business:erp:cost-object:update`：录工序完工率与「填写本期产量与在制」
 *   （`PUT /cost-object/{id}/output`）是同一件事的两半。
 */
export const COST_WIP_PERMISSIONS = {
  /** 列表 / 工序查询 / 余额查询 */
  read: 'business:erp:cost-object:list',
  /** 保存工序完工率（整表覆盖） */
  write: 'business:erp:cost-object:update',
} as const;

/** 列表每页条数（与其它成本页一致；后端上限 200）。 */
export const PAGE_SIZE = 20;

// ==================== 值域（与后端常量 / 库层 CHECK 逐条对应） ====================

/** 完工率标度（`NUMERIC(5,2)`）。 */
export const PCT_SCALE = 2;

/** 权重标度（`NUMERIC(12,4)`）。 */
export const WEIGHT_SCALE = 4;

/** 权重上限（`NUMERIC(12,4)` 的整数位上限）。 */
export const WEIGHT_CEILING = '99999999.9999';

/** 工序序号上限（库 CHECK 同值）。 */
export const MAX_SEQ = 999;

/** 一次最多保存多少道工序（后端 `MAX_ROWS`）。 */
export const MAX_ROWS = 200;

/** 工序名最大字符数（`VARCHAR(64)`）。 */
export const NAME_MAX_CHARS = 64;

/** 备注最大字符数（`VARCHAR(255)`）。 */
export const REMARK_MAX_CHARS = 255;

// ==================== 完工率录入（M5-21） ====================

/** 成本对象抬头（录入抽屉顶部那一块，`OperationObjectView`）。 */
export interface WipOperationObject {
  /** 成本对象 id。 */
  id: string;
  /** 对象号（`CB-yyyyMMdd-NNNNNN`）。 */
  object_no: string;
  /** 对象类型（一期恒 `BATCH`）。 */
  object_type: string;
  /** 产出品编码（实时 join 主数据）。 */
  product_code: string | null;
  /** 产出品名称（实时 join 主数据）。 */
  product_name: string | null;
  /** 产出批号。 */
  lot_no: string | null;
  /** 生产车间（实时原名）。 */
  workshop_dept_name: string | null;
  /** 状态 `OPEN` / `CLOSED` / `CANCELLED`。 */
  status: string;
  /** 计划数量（4 位）。 */
  plan_qty: string;
  /** 累计完工数量（4 位）。 */
  completed_qty: string;
  /** 期末在制数量（4 位）。 */
  wip_qty: string;
  /** 对象级完工率（= 本表加权汇总回写的那个数，2 位）。 */
  completion_pct: string;
  /** 投料方式 `ONE_TIME` / `AS_PROGRESS`。 */
  material_input_mode: string;
  /** 能不能改（状态闸）。 */
  editable: boolean;
  /** 不能改的原因（可改时为 `null`）。 */
  blocked_reason: string | null;
}

/** 一道工序（`OperationRowView`）。 */
export interface WipOperationRow {
  /** 行主键。 */
  id: string;
  /** 工序序号 1–999。 */
  seq: number;
  /** 工序名。 */
  operation_name: string;
  /** 权重（4 位）。 */
  weight: string;
  /** 本工序完工率（2 位）。 */
  completion_pct: string;
  /** 权重占比 %（4 位）。 */
  share_pct: string;
  /** 对综合完工率的贡献（百分点，4 位）。 */
  contribution_pct: string;
  /** 备注。 */
  remark: string | null;
  /** 最后修改时间（RFC3339）。 */
  updated_at: string;
}

/** 加权汇总（`RollupView`；没有工序时整个 `rollup` 是 `null`）。 */
export interface WipRollup {
  /** 工序行数。 */
  row_count: number;
  /** Σ权重（4 位）。 */
  weight_sum: string;
  /** Σ(权重 × 完工率)（4 位）。 */
  weighted_sum: string;
  /** 未收口的商（6 位，只用于解释尾差）。 */
  pct_unrounded: string;
  /** 写回对象级的综合完工率（2 位）。 */
  pct: string;
  /** 对象级当前值（2 位）。 */
  object_pct: string;
  /** 两者是否一致（`false` = 有人用 `PUT /output` 直接改过对象级）。 */
  matched: boolean;
  /** 差额（对象级 − 汇总，带符号，4 位）。 */
  diff: string;
}

/** 工序完工率查询 / 保存的出参（`OperationRes`）。 */
export interface WipOperationRes {
  /** 成本对象抬头。 */
  object: WipOperationObject;
  /** 工序行（按 `seq` 升序，后端定死顺序）。 */
  rows: WipOperationRow[];
  /** 加权汇总（没有工序时 `null`）。 */
  rollup: WipRollup | null;
  /** 本次保存是否有任何写入（GET 恒 `false`）。 */
  changed: boolean;
  /** 本次插入 / 更新 / 软删的行数。 */
  inserted: number;
  /** 本次更新的行数。 */
  updated: number;
  /** 本次软删的行数（整表覆盖下「少一行」= 删一道工序）。 */
  deleted: number;
  /** 录入闸：`editable` / `locked`。 */
  state: string;
  /** 公式说明（后端原文，页脚直接用）。 */
  formula: string;
  /** 权重语义说明（列头 tooltip）。 */
  weight_hint: string;
  /** 只提示不拦的提醒。 */
  warnings: string[];
  /** 页脚说明（这张表现在是什么状态）。 */
  note: string;
}

/** 一道工序的保存入参（`OperationRowReq`）。 */
export interface WipOperationRowReq {
  /** 工序序号（1–999，同一批内唯一，必填）。 */
  seq: number;
  /** 工序名（去空白后 1–64 字，必填）。 */
  operation_name: string;
  /** 权重（缺省 1；必须 > 0，最多 4 位小数）。 */
  weight?: string;
  /** 本工序完工率 0–100（**必填**：缺失后端 400，不会当 0）。 */
  completion_pct: string;
  /** 备注（≤ 255 字，空白按 `null`）。 */
  remark?: string | null;
}

/**
 * 整表保存入参（`OperationSaveReq`）。
 *
 * **payload 里有几行就是几行**：库里存在但没给的 `seq` 会被软删 ——
 * 这是「删掉一道工序」的唯一路径（没有单独的 DELETE 接口）。
 */
export interface WipOperationSaveReq {
  /** 工序行（按 `seq` 对齐，顺序无关；后端会重排）。 */
  rows: WipOperationRowReq[];
}

/** 录入闸的两态（`EditGate::state()`）。 */
export const EDIT_STATE = {
  /** 可改。 */
  EDITABLE: 'editable',
  /** 不可改（已关批 / 已作废 / 已结算）。 */
  LOCKED: 'locked',
} as const;

// ==================== 期末在制余额（M5-23） ====================

/** 余额查询入参（`BalanceQuery`）。 */
export interface WipBalanceQuery {
  /** 期间 `yyyy-MM`（**必填**：余额是截至某一期的存量，后端缺了 400）。 */
  period_code: string;
  /** 关键字（对象号 / 生产订单号 / 批号 / 产出品编码 / 名称）。 */
  keyword?: string;
  /** 状态筛选（缺省 `wip`：只显示有余额的行）。 */
  state?: string;
  /** 页码（从 1 起）。 */
  page?: number;
  /** 每页条数（上限 200）。 */
  page_size?: number;
}

/** 成本核算批次（`BatchView`）。 */
export interface WipBalanceBatch {
  /** 批次 id。 */
  id: string;
  /** 批次号。 */
  batch_no: string;
  /** 批次状态。 */
  status: string;
}

/** 算不出来的原因（`IssueView`：事实 + 怎么修）。 */
export interface WipBalanceIssue {
  /** 稳定码。 */
  code: string;
  /** 事实。 */
  message: string;
  /** 怎么修。 */
  fix: string;
}

/** 一个要素的余额滚动（`ElementRollView`；分要素不可用时金额是 `null`）。 */
export interface WipElementRoll {
  /** 要素码 `MATERIAL` / `LABOR` / `OVERHEAD`。 */
  element: string;
  /** 中文名。 */
  label: string;
  /** 期初在制。 */
  beg: string | null;
  /** 本期投入。 */
  cur: string | null;
  /** 完工转出。 */
  finished: string | null;
  /** 期末在制余额。 */
  wip: string | null;
}

/** 账面快照（已结算那几列；没结算 ⇒ `ledger` 整个是 `null`）。 */
export interface WipLedger {
  /** 计算单状态（`CALC` / `SETTLED`）。 */
  status: string;
  /** 账面期末在制数量（4 位）。 */
  wip_end_qty: string;
  /** 账面期末在制成本（4 位）。 */
  wip_end_cost: string;
  /** 试算与账面一致（`false` ⇒ 分要素不给，见 `note`）。 */
  agrees: boolean;
  /** 一句话说明。 */
  note: string;
}

/** 结转核对（`CarryView`）。 */
export interface WipCarry {
  /** 状态值，见 {@link CARRY_STATES}。 */
  state: string;
  /** 状态中文名。 */
  label: string;
  /** 需要人去处理（页面标红）。 */
  needs_attention: boolean;
  /** 下期期间（没算时是建议的下一期）。 */
  to_period: string | null;
  /** 下期实际取走的期初数量。 */
  to_beg_qty: string | null;
  /** 下期实际取走的期初成本。 */
  to_beg_cost: string | null;
  /** 数量差。 */
  diff_qty: string | null;
  /** 成本差。 */
  diff_cost: string | null;
  /** 一句话说明（含怎么修）。 */
  hint: string;
}

/** 一行余额（`BalanceRow`）。 */
export interface WipBalanceRow {
  /** 成本对象 id。 */
  object_id: string;
  /** 对象号。 */
  object_no: string;
  /** 对象类型。 */
  object_type: string;
  /** 对象状态 `OPEN` / `CLOSED`。 */
  object_status: string;
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
  /** 生产车间 id。 */
  workshop_dept_id: string;
  /** 生产车间名称。 */
  workshop_dept_name: string | null;
  /** 计划数量。 */
  plan_qty: string;
  /** 累计完工数量。 */
  cumulative_completed_qty: string;
  /** 本期完工数量。 */
  completed_qty: string;
  /** 期初在制数量（上期期末）。 */
  beg_qty: string;
  /** 期初在制期间（上期；首期 ⇒ `null`）。 */
  beg_period: string | null;
  /** 期末在制数量。 */
  wip_qty: string;
  /** 完工率 %（2 位）。 */
  completion_pct: string;
  /** 期初在制金额。 */
  beg_total: string | null;
  /** 本期投入。 */
  cur_total: string | null;
  /** 完工转出。 */
  finished_total: string | null;
  /** **期末在制余额**（本表的主角；算不出来 ⇒ `null`）。 */
  wip_total: string | null;
  /** 三个要素的滚动（固定顺序：料 / 工 / 费）。 */
  elements: WipElementRoll[];
  /** 分要素能不能用（三项加起来 = 余额）。 */
  elements_available: boolean;
  /** 口径来源 `settled`（已入账）/ `live`（试算未入账）。 */
  source: string;
  /** 口径来源中文名。 */
  source_label: string;
  /** 账面快照。 */
  ledger: WipLedger | null;
  /** 结转核对。 */
  carry: WipCarry;
  /** 算不出来的原因。 */
  issues: WipBalanceIssue[];
  /** 只提示不拦的提醒。 */
  warnings: string[];
}

/** 合计（`BalanceTotalsView`，整张表，不受分页影响）。 */
export interface WipBalanceTotals {
  /** 行数。 */
  row_count: number;
  /** 有余额的行数。 */
  wip_row_count: number;
  /** 已结转行数。 */
  carried_count: number;
  /** 待结转行数。 */
  pending_count: number;
  /** 结转不齐行数。 */
  mismatch_count: number;
  /** 未入账行数。 */
  unsettled_count: number;
  /** 已红冲行数。 */
  reversed_count: number;
  /** 关批挂账行数。 */
  closed_wip_count: number;
  /** 无余额行数。 */
  cleared_count: number;
  /** 算不出来行数。 */
  blocked_count: number;
  /** 期初在制数量合计。 */
  beg_qty: string;
  /** 期末在制数量合计。 */
  wip_qty: string;
  /** 期初在制金额合计。 */
  beg_total: string;
  /** 本期投入合计。 */
  cur_total: string;
  /** 完工转出合计。 */
  finished_total: string;
  /** 期末在制余额合计。 */
  wip_total: string;
  /** 期末在制按要素（料）。 */
  wip_material: string;
  /** 期末在制按要素（人工）。 */
  wip_labor: string;
  /** 期末在制按要素（制造费用）。 */
  wip_overhead: string;
  /** 未入账的余额合计（`unsettled` 行：试算有、账面没有）。 */
  unbooked_total: string;
  /** 关批挂账的余额合计。 */
  closed_wip_total: string;
  /** 结转不齐的余额合计（金额差取绝对值加总，正负不互相抵消）。 */
  mismatch_abs_total: string;
  /** 分要素可加的行数。 */
  element_row_count: number;
  /** 合计口径的一句话。 */
  note: string;
}

/** 整张余额表出参（`BalanceRes`）。 */
export interface WipBalanceRes {
  /** 期间。 */
  period_code: string;
  /** 当期成本核算批次（没建 / 已红冲 ⇒ `null`）。 */
  batch: WipBalanceBatch | null;
  /** 最近一次红冲的批次（有 ⇒ 本表所有行都按「已红冲」处理）。 */
  reversed_batch: WipBalanceBatch | null;
  /** 是否已建活跃批次。 */
  cost_ready: boolean;
  /** 状态筛选回显。 */
  state: string;
  /** 关键字回显。 */
  keyword: string | null;
  /** 余额公式（后端原文）。 */
  formula_roll: string;
  /** 结转口径（后端原文）。 */
  formula_carry: string;
  /** 合计（整张表，不受分页影响）。 */
  totals: WipBalanceTotals;
  /** 数据页。 */
  list: WipBalanceRow[];
  /** 命中总行数。 */
  total: number;
  /** 页码。 */
  page: number;
  /** 每页条数。 */
  page_size: number;
  /** 可选筛档（后端 `STATE_FILTERS` 原序）。 */
  state_options: Array<{ value: string; label: string }>;
  /** 页脚说明。 */
  note: string;
}

// ==================== 词表：结转八态 ====================

/** 一个结转状态的定义（值 / 中文 / 是否需要人去处理 / 页面语义色）。 */
export interface CarryStateDef {
  /** 后端字面值。 */
  value: string;
  /** 中文名（与后端 `CarryState::label` 逐字一致，2–4 字）。 */
  label: string;
  /** 需要人去处理（后端 `needs_attention`，页面标红）。 */
  attention: boolean;
  /** TDesign tag 主题。 */
  theme: 'success' | 'warning' | 'danger' | 'default' | 'primary';
}

/**
 * 结转八态（**顺序与后端 `CarryState` 声明一致**，不是字典序）。
 *
 * `unsettled`（未入账）是最危险的一档：它**看着有余额**（试算数），但库里没有已结算快照，
 * 下一期算的时候期初取 0 —— 这笔钱会凭空变成下期的完工成本。所以它 `attention = true`。
 */
export const CARRY_STATES: readonly CarryStateDef[] = [
  { value: 'carried', label: '已结转', attention: false, theme: 'success' },
  { value: 'pending', label: '待结转', attention: false, theme: 'primary' },
  { value: 'mismatch', label: '结转不齐', attention: true, theme: 'danger' },
  { value: 'unsettled', label: '未入账', attention: true, theme: 'warning' },
  { value: 'reversed', label: '已红冲', attention: true, theme: 'warning' },
  { value: 'closed_wip', label: '关批挂账', attention: true, theme: 'warning' },
  { value: 'cleared', label: '无余额', attention: false, theme: 'default' },
  { value: 'blocked', label: '算不出', attention: true, theme: 'danger' },
] as const;

/**
 * 状态筛选下拉（**顺序即后端 `STATE_FILTERS`**，前端不重排）。
 *
 * 缺省是 `wip`（只显示有余额的行）—— 与后端 `BalanceStateFilter::parse` 的缺省一致。
 */
export const BALANCE_STATE_FILTERS: readonly { value: string; label: string }[] = [
  { value: 'wip', label: '有余额' },
  { value: 'all', label: '全部' },
  { value: 'unsettled', label: '未入账' },
  { value: 'pending', label: '待结转' },
  { value: 'carried', label: '已结转' },
  { value: 'mismatch', label: '结转不齐' },
  { value: 'closed_wip', label: '关批挂账' },
  { value: 'reversed', label: '已红冲' },
  { value: 'blocked', label: '算不出' },
  { value: 'cleared', label: '无余额' },
] as const;

/** 余额缺省筛档（与后端 `BalanceStateFilter` 缺省一致）。 */
export const BALANCE_STATE_DEFAULT = 'wip';

/** 成本对象状态（录入页筛选用；`CostObjectStatus` 的三态，逐字一致）。 */
export const COST_OBJECT_STATUS_OPTIONS: readonly { value: string; label: string }[] = [
  { value: 'OPEN', label: '在产' },
  { value: 'CLOSED', label: '已关批' },
  { value: 'CANCELLED', label: '已作废' },
] as const;
