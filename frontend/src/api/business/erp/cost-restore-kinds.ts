/**
 * 成本还原页类型与词表（M5-30【M5·成本】成本还原：前端页 · 还原明细）。
 *
 * 一页两个页签，各消费一条后端接口 —— **字段逐字抄后端 DTO，不自己取名**：
 *
 * | 页签 | 接口 | 后端文件 | 本文件的类型 |
 * |---|---|---|---|
 * | 还原明细 | `GET /business/erp/cost-restore-rollup` | `cost_restore_rollup_dto.rs`（M5-27） | {@link RestoreRollupRes} |
 * | 还原后报表 | `GET /business/erp/cost-restore-report` | `cost_restore_report_dto.rs`（M5-28） | {@link RestoreReportRes} |
 *
 * 两张表都**复用** `business:erp:cost-object:list`（与结构表 / 在制余额 / 约当产量同码），
 * 本卡零新增权限码：ERP 全域尚未做「页面码 / 接口码解耦」（PERM-01 只在 HR 试点），
 * 新开 `cost-restore:*` 会立刻变成没有菜单行承载的死码，`permission_audit_tests::n14` 判红。
 *
 * # 数值一律是字符串（前端只展示，不重算）
 *
 * 后端 `rust_decimal::Decimal` 出 JSON 是字符串：金额 `"1000.0000"`（4 位，`money_text`
 * 已去掉尾随 0）、百分比 `"62.5"`（2 位）—— **百分号自带的口径见下**。
 *
 * 前端**不做任何业务算术**：
 *
 * * 逐层卷积（材料里的半成品按所产批次结构递归拆开）在 `cost_restore_rollup_dto::restore_of`；
 * * 按车间 / 产品 / 对象 / 整表的分组汇总与料工费占比在 `cost_restore_report_dto::aggregate`；
 * * 两侧都有单测钉着（M5-27 43 条 / M5-28 24 条 / M5-29 12 条跨表对拍）。
 *
 * 前端再算一遍只会制造两份口径：「页面显示 62.51%、后端 62.50%」这种差异查起来要人命。
 *
 * # 三个最容易读错的地方（页面必须原样转达，不许"美化"）
 *
 * 1. **`share_sum` 可能不是 100.00**：四项占比各自四舍五入到 2 位，合计允许 ±0.01 舍入差
 *    （M5-29 的四层链夹具上实测真实出现 `100.01`："37.83 + 31.09 + 31.09 + 0.00"）。
 *    页面**不许**硬判 `=== 100` 报警，判据见 `cost-restore-logic::shareSumTone`
 *    （容差 {@link SHARE_SUM_TOLERANCE}）；
 * 2. **`未还原不彻底 ≠ 0` 不代表算错**：产出批次没算成本 / 已红冲 / 结构闭环 / 到层数上限，
 *    都会让一部分金额停在半成品形态（`unresolved`），它既不算外购料也不丢掉 ——
 *    「还原后合计 + 还原不彻底 = 还原前合计」才是恒等式（`check` 恒为 0）；
 * 3. **逐层台账各层不能相加**：第 1 层是本期材料投入的去向，第 2 层起是**上一层「材料部分」
 *    往下拨的钱**（`carried_down`），不是新增投入。页面上把 `flows[]` 的 `imputed` 加起来
 *    会得到比投入大好几倍的数字（M5-27 模块头写明）。
 *
 * # `null` 的含义（别当 0 处理）
 *
 * | 字段 | `null` 的含义 |
 * |---|---|
 * | `row.item` / `row.finished_cost` | 本期还没算过这个成本对象的计算单（不是"成本为 0"） |
 * | `row.semi_share` / `semi_share` | 还原前材料为 0（没有分母） |
 * | `after_*_share` / `*_share` / `*_pp` / `share_sum` | 分母为 0，占比无从谈起 |
 * | `semis[].source` / `source_label` | 该节点没有结构来源（取不到结构，已计入不彻底） |
 */

// ==================== 权限码（复用成本对象，零新增） ====================

/**
 * 本页权限码。
 *
 * 与后端 `cost_restore_rollup_dto::PERM_LIST`、`cost_restore_report_dto::PERM_LIST`
 * **逐字相同**（后端 `permission_codes_match_the_cost_object_handler` 与
 * `perm_matrix_tests` 钉住不漂移）。三张还原表（结构 / 还原 / 报表）都是成本对象的一列数，
 * 看得到成本对象的人本来就该看到「这笔材料里的半成品拆成了多少料工费」。
 */
export const COST_RESTORE_PERMISSIONS = {
  /** 明细表 / 报表读取（两条都挂这一个码） */
  read: 'business:erp:cost-object:list',
} as const;

// ==================== 默认值 / 上限（与后端常量逐条对应） ====================

/** 明细表每页行数（后端 `normalize_page` 上限 200；与其它成本页一致给 20）。 */
export const PAGE_SIZE = 20;

/** 报表每页分组数（后端 `PAGE_SIZE_DEFAULT = 50`）。 */
export const REPORT_PAGE_SIZE = 50;

/** 报表每页分组数上限（后端 `PAGE_SIZE_LIMIT = 500`）。 */
export const REPORT_PAGE_SIZE_LIMIT = 500;

/** 默认下钻层数（后端 `MAX_DEPTH_DEFAULT = 6`：覆盖绝大多数离散制造）。 */
export const MAX_DEPTH_DEFAULT = 6;

/** 下钻层数上限（后端 `MAX_DEPTH_LIMIT = 12`；超了后端夹到 12，不报错）。 */
export const MAX_DEPTH_LIMIT = 12;

/** 下钻层数下限（0 在后端表示"用默认值"，页面直接给 1 起）。 */
export const MAX_DEPTH_MIN = 1;

/** 占比合计的目标值（`材料 + 人工 + 制造费用 + 还原不彻底` 应为它）。 */
export const SHARE_SUM_TARGET = 100;

/**
 * 占比合计的容差（百分点）。
 *
 * 四项各自由 4 位金额四舍五入到 2 位，最坏情况合计偏离 2 个 0.01 ⇒ 容差取 0.02。
 * M5-29 的四层链夹具实测 `100.01`（37.83 + 31.09 + 31.09 + 0.00），**是正常值**。
 */
export const SHARE_SUM_TOLERANCE = 0.02;

/** 还原不彻底的金额显示阈值：小于它（且不为 0）就提示"这是舍入尾差量级"用。 */
export const UNRESOLVED_TINY = 0.01;

// ==================== 公共出参形状（两张表共用） ====================

/** 下拉选项（后端 `OptionView`）。 */
export interface OptionView {
  /** 值。 */
  value: string;
  /** 中文名。 */
  label: string;
}

/** 当期成本核算批次（后端 `BatchView`）。 */
export interface BatchView {
  /** 批次 id。 */
  id: string;
  /** 批次号（`CS-yyyyMM-0001`）。 */
  batch_no: string;
  /** 状态（`DRAFT` / `SUBMITTED` / `APPROVED` / `SETTLED` / `CLOSED`）。 */
  status: string;
}

/** 三要素 + 合计（后端 `ElementView`；`total` 恒等于三者之和）。 */
export interface ElementView {
  /** 材料（还原前含半成品；还原后只含外购料）。 */
  material: string;
  /** 人工。 */
  labor: string;
  /** 制造费用。 */
  overhead: string;
  /** 合计。 */
  total: string;
}

/** 取不齐 / 被截断的事实 + 怎么修（后端 `IssueView`）。 */
export interface IssueView {
  /** 稳定码（`NO_STRUCTURE` / `CYCLE` / `DEPTH_CUT` / `PERIOD_AVG` …）。 */
  code: string;
  /** 事实。 */
  message: string;
  /** 怎么修。 */
  fix: string;
}

/** 要素键（三要素固定顺序，页面不按后端返回顺序渲染）。 */
export type ElementKey = 'material' | 'labor' | 'overhead';

/** 要素中文名（列标直接用）。 */
export const ELEMENT_LABELS: Record<ElementKey, string> = {
  material: '材料',
  labor: '人工',
  overhead: '制造费用',
};

/** 三要素的固定渲染顺序（材料 → 人工 → 制造费用）。 */
export const ELEMENT_KEYS: ElementKey[] = ['material', 'labor', 'overhead'];

/** 从 {@link ElementView} 取某一要素的金额文本。 */
export function elementOf(view: ElementView, key: ElementKey): string {
  return view[key];
}

// ==================== 行状态（三档，比结构表五档粗） ====================

/** 一行的还原形状（后端 `RollupState`）。 */
export type RestoreState = 'restored' | 'partial' | 'no_semi';

/** 状态筛选（含"全部"）。 */
export type RestoreStateFilter = 'all' | RestoreState;

/** tag 主题（与 TDesign `t-tag` 的 `theme` 同值域）。 */
export type TagTheme = 'success' | 'warning' | 'danger' | 'default' | 'primary';

/** 状态词表（顺序即下拉顺序，与后端 `STATE_FILTERS` 一致）。 */
export interface RestoreStateOption {
  /** 值。 */
  value: RestoreStateFilter;
  /** 中文名。 */
  label: string;
  /** 行内 tag 主题。 */
  theme: TagTheme;
  /** 一句话解释（tooltip / 展开行用）。 */
  hint: string;
}

/**
 * 三档行状态 + "全部"。
 *
 * 后端的判据（`row_state`）：`semi_count = 0` ⇒ `no_semi`；否则 `unresolved > 0`
 * ⇒ `partial`；否则 `restored`。**前端不从金额反推状态**（反推就会出现
 * 「页面说已还原、后端说 partial」）。
 */
export const RESTORE_STATES: RestoreStateOption[] = [
  {
    value: 'all',
    label: '全部',
    theme: 'default',
    hint: '不筛状态：三档一起看',
  },
  {
    value: 'restored',
    label: '已还原',
    theme: 'success',
    hint: '材料里的自制半成品已逐层拆到外购料 / 人工 / 制造费用，一分不剩',
  },
  {
    value: 'partial',
    label: '不彻底',
    theme: 'danger',
    hint: '有金额停在半成品形态（产出批次没算 / 已红冲 / 结构闭环 / 到层数上限）：看展开行第几层卡住',
  },
  {
    value: 'no_semi',
    label: '无需还原',
    theme: 'default',
    hint: '本期材料全是外购料，没有自制半成品可拆，还原前后一致',
  },
];

/** 状态筛选默认值（`all`：先让人看到全貌）。 */
export const RESTORE_STATE_DEFAULT: RestoreStateFilter = 'all';

// ==================== 汇总维度（报表页签） ====================

/** 报表的分组维度（后端 `GroupBy`）。 */
export type RestoreGroupBy = 'dept' | 'product' | 'object' | 'all';

/** 维度选项（顺序即下拉顺序，与后端 `GROUP_BYS` 一致）。 */
export interface RestoreGroupOption {
  /** 值。 */
  value: RestoreGroupBy;
  /** 中文名（2–4 字，列标直接用）。 */
  label: string;
  /** 一组是什么（表头副标 / tooltip）。 */
  hint: string;
}

/**
 * 四档维度。
 *
 * `object` 一对象一行，用于**和明细表逐行对账**（M5-29 钉住的等式：
 * 报表 `group_by=object` 的每一组金额 == 明细表对应行的 8 个金额字段）。
 */
export const RESTORE_GROUP_BYS: RestoreGroupOption[] = [
  { value: 'dept', label: '按车间', hint: '一组 = 一个生产车间 / 部门（缺车间归「未指定车间」）' },
  { value: 'product', label: '按产品', hint: '一组 = 一个产出品（编码 + 名称）' },
  { value: 'object', label: '按对象', hint: '一组 = 一个成本对象：可与明细表逐行对账' },
  { value: 'all', label: '整表', hint: '一组 = 全部（只有一个「全部」行，翻页没有意义）' },
];

/** 维度默认值（`dept`，与后端缺省一致）。 */
export const RESTORE_GROUP_BY_DEFAULT: RestoreGroupBy = 'dept';

// ==================== 明细表（M5-27 cost-restore-rollup） ====================

/** 逐层台账一行（后端 `FlowView`）。 */
export interface RestoreFlow {
  /** 层级（1 起）。 */
  level: number;
  /** 本层被卷积的节点数。 */
  node_count: number;
  /** 流入本层的金额。 */
  imputed: string;
  /** 本层确认为外购材料。 */
  purchased: string;
  /** 本层确认为人工。 */
  labor: string;
  /** 本层确认为制造费用。 */
  overhead: string;
  /** 本层继续往下卷积的金额（= 本层流入的「材料部分」）。 */
  carried_down: string;
  /** 本层无法继续还原。 */
  unresolved: string;
  /** 本层口径说明（后端文案，原样展示）。 */
  note: string;
}

/** 半成品还原明细一行（后端 `SemiView`）。 */
export interface RestoreSemi {
  /** 层级。 */
  level: number;
  /** 物料 id。 */
  material_id: string;
  /** 物料编码。 */
  material_code: string | null;
  /** 物料名称。 */
  material_name: string | null;
  /** 领料批号。 */
  lot_no: string | null;
  /** 被卷积的金额（父层拨下来的份额）。 */
  imputed: string;
  /** 结构来源（`batch` / `period_avg`；取不到结构时 `null`）。 */
  source: string | null;
  /** 来源中文名。 */
  source_label: string | null;
  /** 产出它的成本对象号（多批用 `/` 连）。 */
  producer_nos: string | null;
  /** 还原结果（材料只含外购料；`total` = 流入 − 不彻底）。 */
  elements: ElementView;
  /** 还原不彻底。 */
  unresolved: string;
  /** 是否停在这一层（不再往下钻）。 */
  stopped: boolean;
}

/** 还原差额（后端 `DeltaView`；`total` 恒为 0）。 */
export interface RestoreDelta {
  /** 材料变化（还原后 − 还原前，通常为负）。 */
  material: string;
  /** 人工变化（通常为正）。 */
  labor: string;
  /** 制造费用变化（通常为正）。 */
  overhead: string;
  /** 合计变化（**恒为 0**：还原不动总额）。 */
  total: string;
  /** 口径说明。 */
  note: string;
}

/** 本期计算单要点（后端 `ItemBrief`；没算过则整块是 `null`）。 */
export interface RestoreItemBrief {
  /** 状态（`CALC` / `SETTLED`）。 */
  status: string;
  /** 完工数量。 */
  completed_qty: string;
  /** 完工转出成本。 */
  finished_cost: string;
  /** 完工单位成本（6 位）。 */
  unit_finished_cost: string;
}

/** 明细表一行（后端 `Row`，一行一个成本对象）。 */
export interface RestoreRow {
  /** 成本对象 id。 */
  object_id: string;
  /** 对象号。 */
  object_no: string;
  /** 对象类型。 */
  object_type: string;
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
  /** 车间名称。 */
  workshop_dept_name: string | null;
  /** 本期计算单（没算过 ⇒ `null`）。 */
  item: RestoreItemBrief | null;
  /** 本期完工成本（没计算单 ⇒ `null`）。 */
  finished_cost: string | null;
  /** 还原前（材料**含**半成品）。 */
  before: ElementView;
  /** 材料里的自制半成品金额。 */
  semi_in_material: string;
  /** 材料里的外购料金额（还原前口径）。 */
  purchased_in_material: string;
  /** 半成品占材料投入的百分比。 */
  semi_share: string | null;
  /** 还原后（材料只含外购料）。 */
  after: ElementView;
  /** 还原后材料占**还原后合计**的百分比。 */
  after_material_share: string | null;
  /** 还原后人工占比。 */
  after_labor_share: string | null;
  /** 还原后制造费用占比。 */
  after_overhead_share: string | null;
  /** 还原差额。 */
  delta: RestoreDelta;
  /** 还原出来的人工（= 还原后人工 − 还原前人工）。 */
  restored_labor: string;
  /** 还原出来的制造费用。 */
  restored_overhead: string;
  /** 还原不彻底的金额。 */
  unresolved: string;
  /** 行状态（`restored` / `partial` / `no_semi`）。 */
  state: string;
  /** 行状态中文名（后端给的，直接用）。 */
  state_label: string;
  /** 还原层数（= 台账层数）。 */
  depth: number;
  /** 节点数。 */
  node_count: number;
  /** 逐层台账。 */
  flows: RestoreFlow[];
  /** 半成品还原明细。 */
  semis: RestoreSemi[];
  /** 取不到 / 被截断的原因。 */
  issues: IssueView[];
  /** 只提示不拦的提醒。 */
  warnings: string[];
}

/** 整表合计（后端 `TotalsView`）。 */
export interface RestoreTotals {
  /** 行数。 */
  row_count: number;
  /** 已还原行数。 */
  restored_row_count: number;
  /** 还原不彻底行数。 */
  partial_row_count: number;
  /** 无需还原行数。 */
  no_semi_row_count: number;
  /** 还原前材料（含半成品）。 */
  before_material: string;
  /** 还原前人工。 */
  before_labor: string;
  /** 还原前制造费用。 */
  before_overhead: string;
  /** 还原前合计。 */
  before_total: string;
  /** 还原后外购材料。 */
  after_material: string;
  /** 还原后人工。 */
  after_labor: string;
  /** 还原后制造费用。 */
  after_overhead: string;
  /** 还原后合计（**不含**还原不彻底）。 */
  after_total: string;
  /** 还原不彻底合计。 */
  unresolved: string;
  /** 材料里的半成品合计。 */
  semi_in_material: string;
  /** 还原后合计 + 不彻底 − 还原前合计（应为 0）。 */
  check: string;
  /** 最深层数。 */
  max_depth: number;
  /** 节点数合计。 */
  node_count: number;
  /** 材料占**还原后合计**的百分比。 */
  after_material_share: string | null;
  /** 人工占还原后合计的百分比。 */
  after_labor_share: string | null;
  /** 制造费用占还原后合计的百分比。 */
  after_overhead_share: string | null;
  /** 合计口径说明。 */
  note: string;
}

/** 明细表整份出参（后端 `RollupRes`；`list / total / page / page_size` 与 `PageData` 同构）。 */
export interface RestoreRollupRes {
  /** 期间。 */
  period_code: string;
  /** 当期成本核算批次（没建 ⇒ `null`）。 */
  batch: BatchView | null;
  /** 是否已建批次。 */
  cost_ready: boolean;
  /** 状态筛选回显。 */
  state: string;
  /** 关键字回显。 */
  keyword: string | null;
  /** 下钻层数回显。 */
  max_depth: number;
  /** 卷积口径。 */
  formula_restore: string;
  /** 不彻底口径。 */
  formula_unresolved: string;
  /** 合计（整表，不受分页影响）。 */
  totals: RestoreTotals;
  /** 行。 */
  list: RestoreRow[];
  /** 筛选后的行数。 */
  total: number;
  /** 页码。 */
  page: number;
  /** 每页行数。 */
  page_size: number;
  /** 状态选项。 */
  state_options: OptionView[];
  /** 页脚说明。 */
  note: string;
}

/** 明细表入参（后端 `RollupQuery`；`period_code` 必填）。 */
export interface RestoreRollupQuery {
  /** 期间 `yyyy-MM`（必填）。 */
  period_code?: string;
  /** 关键字（对象号 / 生产订单号 / 产出批号 / 产出品编码 / 产出品名称）。 */
  keyword?: string;
  /** 状态筛选（`all` / `restored` / `partial` / `no_semi`）。 */
  state?: string;
  /** 页码（1 起）。 */
  page?: number;
  /** 每页行数（后端上限 200）。 */
  page_size?: number;
  /** 下钻层数（默认 6，上限 12）。 */
  max_depth?: number;
}

// ==================== 还原后报表（M5-28 cost-restore-report） ====================

/** 还原前一组（后端 `ShareView`）。 */
export interface RestoreShareView {
  /** 材料（含半成品）。 */
  material: string;
  /** 人工。 */
  labor: string;
  /** 制造费用。 */
  overhead: string;
  /** 合计。 */
  total: string;
  /** 材料占比（分母 = 还原前合计）。 */
  material_share: string | null;
  /** 人工占比。 */
  labor_share: string | null;
  /** 制造费用占比。 */
  overhead_share: string | null;
}

/** 还原后一组（后端 `RestoredShareView`）。 */
export interface RestoreRestoredShareView {
  /** 外购材料。 */
  material: string;
  /** 人工（本期归集 + 从材料里拆出来的）。 */
  labor: string;
  /** 制造费用（同上）。 */
  overhead: string;
  /** 还原后三要素合计（**不含**还原不彻底）。 */
  total: string;
  /** 还原不彻底。 */
  unresolved: string;
  /** 占比分母（= 还原前合计 = 三要素合计 + 不彻底）。 */
  denominator: string;
  /** 材料占比。 */
  material_share: string | null;
  /** 人工占比。 */
  labor_share: string | null;
  /** 制造费用占比。 */
  overhead_share: string | null;
  /** 还原不彻底占比。 */
  unresolved_share: string | null;
}

/** 占比变化（后端 `ShiftView`，单位百分点）。 */
export interface RestoreShiftView {
  /** 材料占比变化（通常为负）。 */
  material_pp: string | null;
  /** 人工占比变化（通常为正）。 */
  labor_pp: string | null;
  /** 制造费用占比变化（通常为正）。 */
  overhead_pp: string | null;
  /** 口径说明。 */
  note: string;
}

/** 报表一组（后端 `GroupRow`：车间 / 产品 / 对象 / 整表合计同形）。 */
export interface RestoreGroupRow {
  /** 分组键（`all` ⇒ `ALL`；合计行也是 `ALL`）。 */
  key: string;
  /** 分组名。 */
  label: string;
  /** 副标（按对象时是「产出品 · 批号」）。 */
  hint: string | null;
  /** 成本对象个数。 */
  object_count: number;
  /** 还原前。 */
  before: RestoreShareView;
  /** 还原后。 */
  restored: RestoreRestoredShareView;
  /** 占比变化。 */
  shift: RestoreShiftView;
  /** 从材料里拆出来的人工（金额）。 */
  restored_labor: string;
  /** 从材料里拆出来的制造费用（金额）。 */
  restored_overhead: string;
  /** 材料里的自制半成品。 */
  semi_in_material: string;
  /** 半成品占材料投入的百分比。 */
  semi_share: string | null;
  /** 已还原行数。 */
  restored_rows: number;
  /** 还原不彻底行数。 */
  partial_rows: number;
  /** 无需还原行数。 */
  no_semi_rows: number;
  /** 占比最高的要素（`material` / `labor` / `overhead` / `none`）。 */
  dominant: string;
  /** 要素中文名（后端给的）。 */
  dominant_label: string;
  /** 四项占比的实际合计（自检：应 ≈100.00，允许 ±0.02 舍入差）。 */
  share_sum: string | null;
  /** 守恒自检：还原后合计 + 不彻底 − 还原前合计（应为 0）。 */
  check: string;
}

/** 报表整份出参（后端 `ReportRes`）。 */
export interface RestoreReportRes {
  /** 期间。 */
  period_code: string;
  /** 当期成本核算批次（没建 ⇒ `null`）。 */
  batch: BatchView | null;
  /** 是否已建批次。 */
  cost_ready: boolean;
  /** 汇总维度回显。 */
  group_by: string;
  /** 汇总维度中文名。 */
  group_by_label: string;
  /** 维度选项。 */
  group_options: OptionView[];
  /** 状态筛选回显。 */
  state: string;
  /** 状态筛选中文名。 */
  state_label: string;
  /** 关键字回显。 */
  keyword: string | null;
  /** 下钻层数回显。 */
  max_depth: number;
  /** 占比分母口径。 */
  basis: string;
  /** 卷积口径（沿用 M5-27）。 */
  formula_restore: string;
  /** 不彻底口径（沿用 M5-27）。 */
  formula_unresolved: string;
  /** 占比公式。 */
  formula_share: string;
  /** 结构变化公式。 */
  formula_shift: string;
  /** 整表合计（不受分组 / 分页影响）。 */
  totals: RestoreGroupRow;
  /** 分组行。 */
  groups: RestoreGroupRow[];
  /** 分组数（= 分页前的总数）。 */
  total: number;
  /** 页码。 */
  page: number;
  /** 每页分组数。 */
  page_size: number;
  /** 状态选项。 */
  state_options: OptionView[];
  /** 页脚说明。 */
  note: string;
}

/** 报表入参（后端 `ReportQuery`；`period_code` 必填）。 */
export interface RestoreReportQuery {
  /** 期间 `yyyy-MM`（必填）。 */
  period_code?: string;
  /** 关键字。 */
  keyword?: string;
  /** 汇总维度（缺省 `dept`；**拼错后端 400**，不静默回落）。 */
  group_by?: string;
  /** 状态筛选。 */
  state?: string;
  /** 页码（1 起）。 */
  page?: number;
  /** 每页分组数（默认 50，上限 500）。 */
  page_size?: number;
  /** 下钻层数（默认 6，上限 12）。 */
  max_depth?: number;
}
