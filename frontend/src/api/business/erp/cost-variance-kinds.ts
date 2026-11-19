/**
 * 差异分析页类型与词表（M5-19 · 消费 M5-11 的只读报表 `GET /report/cost-variance`）。
 *
 * # 与后端一一对应（字段逐字抄 `cost_standard_compare_dto.rs`，不自己取名）
 *
 * | 后端 | 前端 |
 * |---|---|
 * | `CompareRes` | {@link CostVarianceRes} |
 * | `CompareRow` | {@link CostVarianceRow} |
 * | `StandardView` | {@link CostVarianceStandardSide} |
 * | `ActualView` | {@link CostVarianceActualSide} |
 * | `VarianceView` | {@link CostVarianceVariance} |
 * | `ElementView` | {@link CostVarianceElement} |
 * | `TotalsView` | {@link CostVarianceTotals} |
 * | `BatchView` | {@link CostVarianceBatch} |
 *
 * # 数值口径（前端**只展示**，不重算任何公式）
 *
 * | 后端字段族 | JSON 形状 | 前端怎么显示 |
 * |---|---|---|
 * | 金额（`*_amount` / `book_amount` / `rounding_diff` …） | `"10560.0000"`（4 位） | 千分位 + 2 位（{@link moneyText}） |
 * | 单价 / 单位成本（`std_*_cost` / `unit_*`） | `"12.000000"`（6 位） | 去尾零（{@link unitText}） |
 * | 数量 / 工时（`completed_qty` / `std_labor_hours`） | `"800.0000"`（4 位） | 去尾零 + 千分位 |
 * | 差异率（`rate`） | `"0.150000"` = 15% ｜ `null` = 标准额为 0 不计算 | 百分比（{@link rateText}） |
 *
 * **差异率是「比值」不是「百分数」**：后端 `rate_of` 出的是 `差异额 / 标准应发生额` 的原值
 * （0.15 就是 15%），前端乘 100 只是为了显示。负数率照样显示负号（有利差异 + 标准额为正）。
 *
 * # 方向约定（库层 `ck_biz_cost_var_sign` 与 DTO 同口径）
 *
 * ```
 * 正数 = 不利（UNFAVORABLE，超支）  → 红
 * 负数 = 有利（FAVORABLE，节约）    → 绿
 * 零   = 持平（EVEN）               → 灰
 * ```
 *
 * 前端**不按金额符号自己判方向**：后端每行 / 每个要素都给了 `direction` 与 `direction_label`，
 * 页面照抄（符号判定一旦前端自己写一遍，遇到 `-0.0000` 这种就会与前端的显示打架）。
 *
 * # 三要素 = 卡片里的「三类差异」
 *
 * `ELEMENTS` = 材料（MATERIAL）/ 人工（LABOR）/ 制造费用（OVERHEAD），顺序与后端
 * `cost_standard_compare_dto::ELEMENTS` 一致；行内与合计块都是这三行，**不要**按字面值排序
 * （`LABOR < MATERIAL < OVERHEAD` 的字典序会把「料工费」的顺序打乱）。
 */

// ==================== 权限码（复用 cost-method:list，零新增） ====================

/**
 * 本页权限码：**复用** `business:erp:cost-method:list`。
 *
 * 与 M5-11 的后端注解逐字一致（`cost_standard_compare_handler.rs::PERM_LIST`）：
 * 差异报表读的就是「标准成本卡 + 成本计算单」，看得到这两样的人本来就是同一批
 * （财务经理 / 财务总监）。新开 `cost-variance:list` 属于「页面码 / 接口码解耦」，
 * 那是 ERP 级的独立决定（PERM-01 只在 HR 试点），不在本卡范围。
 */
export const COST_VARIANCE_PERMISSION = 'business:erp:cost-method:list';

/** 列表每页条数（与其它成本页一致；后端上限 200）。 */
export const PAGE_SIZE = 20;

// ==================== 请求 ====================

/** `GET /business/erp/report/cost-variance` 入参。 */
export interface CostVarianceQuery {
  /** 期间 `yyyy-MM`（**必填**：报表跨期没有默认值，缺了后端 400）。 */
  period_code: string;
  /** 物料编码 / 名称关键字（可选）。 */
  keyword?: string;
  /** 状态筛选 `all` / `comparable` / `no_standard` / `no_actual`（缺省 `all`）。 */
  state?: string;
  /** 页码（缺省 1）。 */
  page?: number;
  /** 每页条数（缺省 20，上限 200）。 */
  page_size?: number;
}

// ==================== 出参 ====================

/** 标准一侧（无已发布版本 ⇒ 整个 `standard` 为 `null`）。 */
export interface CostVarianceStandardSide {
  /** 卡头 id。 */
  id: string;
  /** 版本号。 */
  version_no: number;
  /** 生效起始日。 */
  effective_from: string;
  /** 停止日（`null` = 至今有效）。 */
  effective_to: string | null;
  /** 最近一次发布时刻（RFC3339）。 */
  published_at: string | null;
  /** 期间内是否换过版（本期还有别的已发布版本生效过）。 */
  switch_in_period: boolean;
  /** 单位标准材料成本（6 位）。 */
  std_material_cost: string;
  /** 单位标准人工成本。 */
  std_labor_cost: string;
  /** 单位标准制造费用。 */
  std_overhead_cost: string;
  /** 单位标准总成本。 */
  std_total_cost: string;
  /** 单位标准工时（4 位）。 */
  std_labor_hours: string;
  /** 标准工资率（6 位）。 */
  std_labor_rate: string;
}

/** 实际一侧（本期无成本计算单 ⇒ 整个 `actual` 为 `null`）。 */
export interface CostVarianceActualSide {
  /** 本期涉及的计算单行数（批次数）。 */
  object_count: number;
  /** 本期完工数量（4 位）。 */
  completed_qty: string;
  /** 账面完工成本（Σ `finished_cost`，4 位）。 */
  book_amount: string;
  /** 完工口径实际材料成本。 */
  amount_material: string;
  /** 完工口径实际人工成本。 */
  amount_labor: string;
  /** 完工口径实际制造费用。 */
  amount_overhead: string;
  /** 完工口径实际成本合计。 */
  amount_total: string;
  /** 单位实际材料成本（6 位）。 */
  unit_material: string;
  /** 单位实际人工成本。 */
  unit_labor: string;
  /** 单位实际制造费用。 */
  unit_overhead: string;
  /** 单位实际成本合计。 */
  unit_total: string;
  /** 本期投入合计（含留在在制的部分，仅参考）。 */
  period_input: string;
  /** 期末在制成本（仅参考）。 */
  wip_end_cost: string;
}

/** 要素级差异（行内与合计共用；合计块的三个 `*_unit` 是空串）。 */
export interface CostVarianceElement {
  /** 要素字面值 `MATERIAL` / `LABOR` / `OVERHEAD`。 */
  element: string;
  /** 要素文本（后端已给，页面优先用它）。 */
  element_label: string;
  /** 单位标准成本（合计块空串）。 */
  std_unit: string;
  /** 单位实际成本（合计块空串）。 */
  act_unit: string;
  /** 单位差异（合计块空串）。 */
  unit_amount: string;
  /** 标准应发生额。 */
  std_amount: string;
  /** 实际完工成本。 */
  act_amount: string;
  /** 差异额（正 = 不利）。 */
  amount: string;
  /** 差异率（`null` = 标准应发生额为 0）。 */
  rate: string | null;
  /** 方向 `UNFAVORABLE` / `FAVORABLE` / `EVEN`。 */
  direction: string;
  /** 方向文本。 */
  direction_label: string;
}

/** 差异块（只有可比行才有；不可比 ⇒ `null`）。 */
export interface CostVarianceVariance {
  /** 完工数量（比较基数，4 位）。 */
  completed_qty: string;
  /** 标准应发生额合计。 */
  std_amount: string;
  /** 实际完工成本合计。 */
  act_amount: string;
  /** 差异额（正 = 不利）。 */
  amount: string;
  /** 差异率（`null` = 标准额为 0）。 */
  rate: string | null;
  /** 方向。 */
  direction: string;
  /** 方向文本。 */
  direction_label: string;
  /** 单位标准成本合计（6 位）。 */
  unit_std_amount: string;
  /** 单位实际成本合计。 */
  unit_act_amount: string;
  /** 单位差异。 */
  unit_amount: string;
  /** 三要素差异（顺序 = 料 / 工 / 费）。 */
  elements: CostVarianceElement[];
  /** 尾差（对平用，**不是**差异）。 */
  rounding_diff: string;
}

/** 逐物料对比一行。 */
export interface CostVarianceRow {
  /** 物料 id。 */
  material_id: string;
  /** 物料编码（物料被删后为 `null`）。 */
  material_code: string | null;
  /** 物料名称。 */
  material_name: string | null;
  /** 物料规格。 */
  material_spec: string | null;
  /** 状态 `BOTH` / `NO_STANDARD` / `NO_ACTUAL`。 */
  state: string;
  /** 状态文本。 */
  state_label: string;
  /** 标准一侧。 */
  standard: CostVarianceStandardSide | null;
  /** 实际一侧。 */
  actual: CostVarianceActualSide | null;
  /** 差异块（不可比 ⇒ `null`）。 */
  variance: CostVarianceVariance | null;
  /** 本行该怎么看 / 怎么修（空串 = 无需说明）。 */
  hint: string;
}

/** 整表合计（**按筛选后的全集**，不是当前页）。 */
export interface CostVarianceTotals {
  /** 行数（物料数）。 */
  rows: number;
  /** 可比行数。 */
  comparable_rows: number;
  /** 缺标准的行数。 */
  no_standard_rows: number;
  /** 缺实际的行数。 */
  no_actual_rows: number;
  /** 可比行完工数量合计（4 位）。 */
  completed_qty: string;
  /** 标准应发生额合计。 */
  std_amount: string;
  /** 实际完工成本合计。 */
  act_amount: string;
  /** 差异额合计（正 = 不利）。 */
  amount: string;
  /** 差异率合计（`null` = 标准额合计为 0）。 */
  rate: string | null;
  /** 方向。 */
  direction: string;
  /** 方向文本。 */
  direction_label: string;
  /** 缺标准行的实际完工成本合计（不可比，参考）。 */
  no_standard_amount: string;
  /** 可比行账面完工成本合计。 */
  book_amount: string;
  /** 尾差合计（对平用）。 */
  rounding_diff: string;
  /** 三要素合计（顺序 = 料 / 工 / 费）。 */
  elements: CostVarianceElement[];
}

/** 当期活跃批次（实际成本的出处）。 */
export interface CostVarianceBatch {
  /** 批次 id。 */
  id: string;
  /** 批次号 `CS-yyyyMM-0001`。 */
  batch_no: string;
  /** 状态 `DRAFT` / `SUBMITTED` / `APPROVED` / `SETTLED` / `CLOSED`。 */
  status: string;
}

/** 报表出参（`GET /report/cost-variance`）。 */
export interface CostVarianceRes {
  /** 期间 `yyyy-MM`。 */
  period_code: string;
  /** 期间首日。 */
  period_start: string;
  /** 期间末日 —— 标准成本按这一天取版本。 */
  period_end: string;
  /** 标准一侧的取数口径（后端固定文案，页面直接展示）。 */
  standard_basis: string;
  /** 当期活跃批次（没有 ⇒ `null`）。 */
  batch: CostVarianceBatch | null;
  /** 实际一侧是否就绪（有活跃批次即有计算单可读）。 */
  cost_ready: boolean;
  /** 本次筛选回显。 */
  state_filter: string;
  /** 关键字回显。 */
  keyword: string | null;
  /** 当前页行。 */
  list: CostVarianceRow[];
  /** 命中总数（按筛选后）。 */
  total: number;
  /** 当前页。 */
  page: number;
  /** 每页条数。 */
  page_size: number;
  /** 整表合计。 */
  totals: CostVarianceTotals;
  /** 口径说明（符号约定 / 不入账 / D-15；可能含 markdown `**`）。 */
  note: string;
}

// ==================== 词表（与后端常量逐一对应） ====================

/** 三要素字面值（顺序 = 料 / 工 / 费，与后端 `ELEMENTS` 一致）。 */
export const ELEMENT_ORDER: readonly string[] = ['MATERIAL', 'LABOR', 'OVERHEAD'];

/** 三要素中文（后端 `element_label` 缺失时的兜底）。 */
export const ELEMENT_LABELS: Readonly<Record<string, string>> = {
  MATERIAL: '材料',
  LABOR: '人工',
  OVERHEAD: '制造费用',
};

/** 行状态字面值。 */
export const ROW_STATE = {
  BOTH: 'BOTH',
  NO_STANDARD: 'NO_STANDARD',
  NO_ACTUAL: 'NO_ACTUAL',
} as const;

/** 行状态中文（后端 `state_label` 缺失时的兜底）。 */
export const ROW_STATE_LABELS: Readonly<Record<string, string>> = {
  BOTH: '可比',
  NO_STANDARD: '缺标准',
  NO_ACTUAL: '缺实际',
};

/** 方向字面值。 */
export const DIRECTION = {
  UNFAVORABLE: 'UNFAVORABLE',
  FAVORABLE: 'FAVORABLE',
  EVEN: 'EVEN',
} as const;

/** 方向中文（后端 `direction_label` 缺失时的兜底）。 */
export const DIRECTION_LABELS: Readonly<Record<string, string>> = {
  UNFAVORABLE: '不利',
  FAVORABLE: '有利',
  EVEN: '持平',
};

/**
 * 状态筛选下拉（值 = 后端 `StateFilter::as_str()` 的字面值）。
 *
 * `comparable` 的标签必须写「**可比且有差异**」而不是「可比」：后端那一档的 WHERE 是
 * `可比 AND variance_amount <> 0`（`StateFilter::Comparable` 的文档注释与单测
 * `state_filter_sql_shapes` 钉着）。只写「可比」会让「差异正好为 0 的可比行」莫名消失。
 *
 * `all` 的标签写「全部状态」而不是「全部」：这一栏与「缺标准 / 缺实际」并排，
 * 只写「全部」会被读成「全部物料」。
 */
export const STATE_OPTIONS: ReadonlyArray<{ label: string; value: string }> = [
  { label: '全部状态', value: 'all' },
  { label: '可比且有差异', value: 'comparable' },
  { label: '缺标准', value: 'no_standard' },
  { label: '缺实际', value: 'no_actual' },
];

/** 状态字面值 → 中文（未知值原样返回，别把 `BOTH` 直接糊在界面上）。 */
export function rowStateLabel(state: string | null | undefined): string {
  const key = (state ?? '').trim();
  return ROW_STATE_LABELS[key] ?? (key || '—');
}

/**
 * 状态 → TDesign 标签主题。
 *
 * | 状态 | 主题 | 理由 |
 * |---|---|---|
 * | `BOTH` | `success` | 两边都有，能比 |
 * | `NO_STANDARD` | `danger` | 没标准就**没法比**，是本页要催的事 |
 * | `NO_ACTUAL` | `warning` | 没实际多半是没做成本核算 / 本期没完工 |
 */
export function rowStateTheme(state: string | null | undefined): 'success' | 'warning' | 'danger' | 'default' {
  switch ((state ?? '').trim()) {
    case ROW_STATE.BOTH:
      return 'success';
    case ROW_STATE.NO_STANDARD:
      return 'danger';
    case ROW_STATE.NO_ACTUAL:
      return 'warning';
    default:
      return 'default';
  }
}

/** 方向字面值 → 中文。 */
export function directionLabel(direction: string | null | undefined): string {
  const key = (direction ?? '').trim();
  return DIRECTION_LABELS[key] ?? (key || '—');
}

/**
 * 方向 → TDesign 标签主题（**不利 = 红、有利 = 绿、持平 = 灰**）。
 *
 * 与常见配色相反的风险点说清楚：这里红色代表「超支」，不是「出错」。
 */
export function directionTheme(direction: string | null | undefined): 'danger' | 'success' | 'default' {
  switch ((direction ?? '').trim()) {
    case DIRECTION.UNFAVORABLE:
      return 'danger';
    case DIRECTION.FAVORABLE:
      return 'success';
    default:
      return 'default';
  }
}

/** 元素字面值 → 中文。 */
export function elementLabel(element: string | null | undefined): string {
  const key = (element ?? '').trim();
  return ELEMENT_LABELS[key] ?? (key || '—');
}
