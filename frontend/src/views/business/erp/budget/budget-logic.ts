/**
 * 预算三页（编制 / 执行 / 科目映射）的纯逻辑 —— 状态口径 / 12 月分解 / 校验 / 请求体。
 *
 * # 为什么单独一个 `.ts`
 *
 * 与 `stock-take-logic.ts` / `purchase-logic.ts` 同理：单测跑在没有 DOM 的 node 里
 * （`pnpm test:unit` = `tsx --test`），`.vue` 的 `setup()` 起不来，而下面这些**错了会出事**：
 *
 * 1. **12 个月分解必须 Σ = 年度额度**，缺月 / 重月 / 负数后端全 400，且报错只点名第一处；
 * 2. **尾差只能记 12 月**（后端文案就是这么说的），前端自己拍一个「四舍五入」会让
 *    最后一分钱凭空多出来或消失；
 * 3. **`MATERIAL_TYPE` 的目标是「物料类型编码」而不是 id**：物料类型是
 *    `biz_material.material_type` 上的字符串枚举（RAW / SEMI / FINISHED / PACKAGING /
 *    CONSUMABLE / OFFICE / SERVICE），全库没有物料类型表、没有 uuid。后端
 *    `budget_dto::validate_mapping` 对这一档要求「`ref_id` 必须为空 + `ref_code` 给类型编码」，
 *    `budget_model::resolve_for_po` 也按 `ref_code` 命中（大小写不敏感），
 *    `uk_biz_bsm_target` 的目标键是 `coalesce(ref_id, upper(btrim(ref_code)))`
 *    —— 三处口径从 t_ce73ae26 起一致，前端只要别给这一档拼 `ref_id` 就行；
 * 4. **映射列表的类型筛选参数名是 `status`**（后端复用 `BudgetFilter`），写错就静默不筛；
 * 5. **动作按钮只能按状态给**：`ACTIVE` 没有「编辑」这条边（ARCH-6 硬规则 4），
 *    给了就是必 409 的按钮。
 *
 * # 金额一律走整数分
 *
 * 后端 `Decimal` 出 JSON 是字符串（`"360000.0000"`），`0.1 + 0.2 !== 0.3`。所以本文件
 * 里所有比较 / 累加 / 分摊都用 {@link toCents} 换成整数分，只在渲染前换回文本 ——
 * 「12 个月加起来等不等于年度额度」是**精确**判断，不能用浮点。
 */

import type {
  AdjustBudgetReq,
  BudgetChangeType,
  BudgetDetail,
  BudgetFilterQuery,
  BudgetMapType,
  BudgetMappingReq,
  BudgetMappingRow,
  BudgetPeriodLine,
  BudgetPeriodReq,
  BudgetReportRow,
  BudgetRow,
  BudgetUsageRow,
  CreateBudgetReq,
  UpdateBudgetReq,
} from '@/api/business/erp/budget';
import { formatCents, toCents, toNumber } from '@/views/business/erp/shared/money';

// ==================== 权限码（`sys_menu.auth_code`，V0227 种了 8 个） ====================

/**
 * 预算的 8 个权限码（逐字抄 `V0227__erp_budget_tables.up.sql` 与 handler 注解）。
 *
 * 按钮显隐只认这一份；后端 `SUPER_ADMIN` 短路全部校验，所以超管永远看得见按钮。
 */
export const BUDGET_PERMS = {
  list: 'business:erp:budget:list',
  create: 'business:erp:budget:create',
  update: 'business:erp:budget:update',
  delete: 'business:erp:budget:delete',
  submit: 'business:erp:budget:submit',
  adjust: 'business:erp:budget:adjust',
  report: 'business:erp:budget:report',
  mapping: 'business:erp:budget:mapping',
} as const;

/** 页面实际要问的权限（`useUserStore().hasPermission` 的入参形状） */
export interface BudgetPermissions {
  /** 可新建 */
  create: boolean;
  /** 可改草稿 */
  update: boolean;
  /** 可删草稿 */
  delete: boolean;
  /** 可提交审批 */
  submit: boolean;
  /** 可调整（新增版本） */
  adjust: boolean;
}

// ==================== 状态 / 类型 / 策略 ====================

/** 状态筛选下拉（顺序按生命周期，不是字母序） */
export const BUDGET_STATUS_OPTIONS: ReadonlyArray<{ label: string; value: BudgetRow['status'] }> = [
  { label: '草稿', value: 'DRAFT' },
  { label: '审批中', value: 'SUBMITTED' },
  { label: '已生效', value: 'ACTIVE' },
  { label: '已被取代', value: 'SUPERSEDED' },
  { label: '已关闭', value: 'CLOSED' },
  { label: '已作废', value: 'CANCELLED' },
];

/** 状态 → 中文（后端 `status_label` 缺失时的兜底，与 `budget_dto::status_label` 逐字一致） */
const STATUS_LABELS: Record<string, string> = Object.fromEntries(
  BUDGET_STATUS_OPTIONS.map((item) => [item.value, item.label]),
);

/** 状态 → 文案（优先用后端给的 `status_label`：口径只有一个来源） */
export function budgetStatusLabel(status: string | null | undefined, backendLabel?: string | null): string {
  if (backendLabel) {
    return backendLabel;
  }
  return STATUS_LABELS[String(status ?? '')] ?? String(status ?? '—');
}

/** 标签主题（TDesign `t-tag` 的 theme 枚举子集） */
export type BudgetTagTheme = 'default' | 'primary' | 'success' | 'warning' | 'danger';

/** 状态 → 主题：草稿灰、审批中蓝、生效绿、被取代/关闭中性、作废红 */
export function budgetStatusTheme(status: string | null | undefined): BudgetTagTheme {
  switch (status) {
    case 'DRAFT':
      return 'default';
    case 'SUBMITTED':
      return 'primary';
    case 'ACTIVE':
      return 'success';
    case 'SUPERSEDED':
    case 'CLOSED':
      return 'warning';
    case 'CANCELLED':
      return 'danger';
    default:
      return 'default';
  }
}

/** 编制 / 调整下拉（列表筛选与详情都要用） */
export const CHANGE_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: BudgetChangeType }> = [
  { label: '编制', value: 'ORIGINAL' },
  { label: '调整', value: 'ADJUST' },
];

/** 编制 / 调整 → 中文 */
export function changeTypeLabel(value: string | null | undefined): string {
  const hit = CHANGE_TYPE_OPTIONS.find((item) => item.value === value);
  return hit ? hit.label : String(value ?? '—');
}

/** 编制 / 调整 → 标签主题：调整是「改额度」，给个警示色好认 */
export function changeTypeTheme(value: string | null | undefined): BudgetTagTheme {
  return value === 'ADJUST' ? 'warning' : 'default';
}

/**
 * 控制策略下拉。
 *
 * `INHERIT` 是**前端造的值**（后端没有这个枚举）：它对应请求里 `control_mode: null`
 * ——「留空 = 继承 `sys_config erp.budget.control_mode`」。UI 上必须显式给出这一项，
 * 否则用户看到空白下拉不知道是「没设」还是「继承」。
 */
export const CONTROL_MODE_OPTIONS: ReadonlyArray<{ label: string; value: 'INHERIT' | 'OFF' | 'WARN' | 'BLOCK' }> = [
  { label: '继承全局', value: 'INHERIT' },
  { label: 'OFF（不校验）', value: 'OFF' },
  { label: 'WARN（超支照常提交 + 标记）', value: 'WARN' },
  { label: 'BLOCK（超支拒绝提交）', value: 'BLOCK' },
];

/** 控制策略 → 中文（`null` / `''` 都算继承） */
export function controlModeLabel(value: string | null | undefined): string {
  if (!value) {
    return '继承全局';
  }
  const hit = CONTROL_MODE_OPTIONS.find((item) => item.value === value);
  return hit ? hit.label : String(value);
}

/** 表单里的策略值 → 请求体值（`INHERIT` / 空 → `null`，后端只认 OFF/WARN/BLOCK） */
export function toControlModePayload(value: string | null | undefined): string | null {
  const text = String(value ?? '').trim();
  if (!text || text === 'INHERIT') {
    return null;
  }
  return text;
}

/** 请求体的策略值 → 表单里的值（`null` → `INHERIT`） */
export function fromControlModePayload(value: string | null | undefined): 'INHERIT' | 'OFF' | 'WARN' | 'BLOCK' {
  const text = String(value ?? '').trim();
  if (text === 'OFF' || text === 'WARN' || text === 'BLOCK') {
    return text;
  }
  return 'INHERIT';
}

// ==================== 12 个月分解（编制的核心） ====================

/** 一行月度草稿（UI 上的 12 个输入框） */
export interface MonthDraftRow {
  /** 月份 1..12 */
  month: number;
  /** 期间 `yyyy-MM`（由年度 + 月份拼，改年度要整组重算） */
  period_code: string;
  /** 额度文本（用户输入的原始字符串，`''` 视为 0） */
  amount: string;
}

/** 月份 → `yyyy-MM` */
export function periodCodeOf(fiscalYear: number, month: number): string {
  return `${fiscalYear}-${String(month).padStart(2, '0')}`;
}

/** 空白 12 行（全 `''`，用于「手工」模式） */
export function emptyMonthRows(fiscalYear: number): MonthDraftRow[] {
  return Array.from({ length: 12 }, (_, index) => ({
    month: index + 1,
    period_code: periodCodeOf(fiscalYear, index + 1),
    amount: '',
  }));
}

/** 整数分 → 输入框文本（固定两位，`12345` → `123.45`） */
export function centsToText(cents: number): string {
  const negative = cents < 0;
  const fixed = (Math.abs(Math.round(cents)) / 100).toFixed(2);
  return negative ? `-${fixed}` : fixed;
}

/**
 * 平均分摊（**尾差记 12 月**，与后端 `budget_dto::even_split` 同口径）。
 *
 * 后端也是 `(total / 12).round_dp(2)` + 第 12 月吃掉尾差，所以「页面点平均分摊 → 直接保存」
 * 一定能让 `Σ = 年度额度`；若这里改成各月独立四舍五入，Σ 会比年度额度少/多几分，
 * 保存时吃后端 400「12 个月合计 ≠ 年度额度」。
 */
export function evenSplitRows(fiscalYear: number, totalAmount: string | number): MonthDraftRow[] {
  const totalCents = toCents(totalAmount);
  const baseCents = Math.round(totalCents / 12);
  const rows = emptyMonthRows(fiscalYear);
  let assigned = 0;
  return rows.map((row) => {
    const cents = row.month === 12 ? totalCents - assigned : baseCents;
    assigned += cents;
    return { ...row, amount: centsToText(cents) };
  });
}

/**
 * 按上年月度占比分摊（ARCH-6 §3.4.1 的「按上年实际比例分摊」的前端实现）。
 *
 * # 口径与偏差（诚实说明）
 *
 * 设计文档写的来源是 `biz_voucher_entry` 按 `account_id + aux_dept_id + period` 汇总的上年
 * 12 个月金额，但本期**没有**暴露「分录按部门聚合」的接口（`grep -n 'entry/agg'
 * backend/src` 零命中），所以页面拿不到发生额。本函数吃的是**上年同槽 ACTIVE 预算的
 * 月度额度**：它同样反映「这家公司在这个科目上的季节性」，且数据就在 `/budget/{id}` 里。
 *
 * 分摊用**最大余数法**（不是逐月四舍五入）：逐月 round 会让 Σ 漂几分；最大余数法能保证
 * `Σ 恰好 = 年度额度`，一分不差 —— 否则用户点了按钮还要被后端 400 打回来。
 *
 * @param fiscalYear 新年度
 * @param totalAmount 新的年度额度
 * @param priorAmounts 上年 12 个月的额度（按 1..12 月顺序，长度必须 12）
 */
export function priorRatioRows(
  fiscalYear: number,
  totalAmount: string | number,
  priorAmounts: ReadonlyArray<string | number>,
): { rows: MonthDraftRow[]; basis: 'prior' | 'even'; note: string } {
  const priorCents = priorAmounts.map((value) => Math.max(0, toCents(value)));
  const weight = priorCents.reduce((sum, cents) => sum + cents, 0);
  if (priorAmounts.length !== 12 || weight <= 0) {
    return {
      rows: evenSplitRows(fiscalYear, totalAmount),
      basis: 'even',
      note: '上年同槽没有可用的月度额度，已退化为「平均分摊」',
    };
  }
  const totalCents = toCents(totalAmount);
  const raw = priorCents.map((cents) => (totalCents * cents) / weight);
  const floors = raw.map((value) => Math.floor(value));
  let leftover = totalCents - floors.reduce((sum, value) => sum + value, 0);
  // 余数按「小数部分从大到小」发；同分给月份小的，保证结果可复现（不依赖数组稳定性）
  const order = raw
    .map((value, index) => ({ index, fraction: value - Math.floor(value) }))
    .sort((a, b) => (b.fraction - a.fraction) || (a.index - b.index));
  for (const item of order) {
    if (leftover <= 0) {
      break;
    }
    floors[item.index] += 1;
    leftover -= 1;
  }
  const rows = emptyMonthRows(fiscalYear).map((row) => ({
    ...row,
    amount: centsToText(floors[row.month - 1] ?? 0),
  }));
  return {
    rows,
    basis: 'prior',
    note: '按上年同槽预算的月度占比分摊（最大余数法，合计与年度额度一分不差）',
  };
}

/** 12 行合计（整数分） */
export function sumMonthCents(rows: ReadonlyArray<{ amount: string }>): number {
  return rows.reduce((sum, row) => sum + toCents(row.amount), 0);
}

/** 12 行合计文本（页脚「合计」用） */
export function sumMonthText(rows: ReadonlyArray<{ amount: string }>): string {
  return formatCents(sumMonthCents(rows));
}

/** 校验结果：`ok` 为 false 时 `errors` 逐条给用户看（后端也是逐条 400，这里前置） */
export interface MonthDraftCheck {
  /** 全部通过 */
  ok: boolean;
  /** 逐条错误（顺序与 UI 展示顺序一致） */
  errors: string[];
  /** 12 行合计（分） */
  sumCents: number;
  /** 年度额度（分） */
  totalCents: number;
  /** 差额（分，= 合计 − 额度；正数 = 多出来，负数 = 还差） */
  diffCents: number;
}

/**
 * 校验「年度额度 + 12 行分解」，镜像 `budget_dto::validate_periods`。
 *
 * 后端规则（逐条对应，一条不少）：
 * - 额度不能为负（"要减额度请走调整"）；
 * - 正好 12 行；
 * - 月份 1..12，不许重复，不许缺月；
 * - 单月额度不能为负；
 * - `Σ = 年度额度`（尾差记 12 月）。
 *
 * ⚠️ 后端**只校验 Σ**，不校验「哪个月多少」；这里的错误文案与后端同口径，
 * 免得用户在页面看到一套说法、在接口里又看到另一套。
 */
export function checkMonthDraft(params: {
  fiscalYear: number;
  totalAmount: string | number;
  rows: ReadonlyArray<MonthDraftRow>;
}): MonthDraftCheck {
  const totalCents = toCents(params.totalAmount);
  const sumCents = sumMonthCents(params.rows);
  const errors: string[] = [];
  if (totalCents < 0) {
    errors.push('年度额度不能为负（预算额度是「批准多少钱」，减额度请走调整）');
  }
  if (params.rows.length !== 12) {
    errors.push(`月度分解必须正好 12 行（当前 ${params.rows.length} 行）`);
  }
  const seen = new Set<number>();
  for (const row of params.rows) {
    if (!Number.isInteger(row.month) || row.month < 1 || row.month > 12) {
      errors.push(`月份 ${row.month} 越界（只能是 1..12）`);
      continue;
    }
    if (seen.has(row.month)) {
      errors.push(`期间重复：${periodCodeOf(params.fiscalYear, row.month)} 出现了不止一次`);
    }
    seen.add(row.month);
    if (toCents(row.amount) < 0) {
      errors.push(`${periodCodeOf(params.fiscalYear, row.month)} 的月度额度不能为负`);
    }
  }
  if (params.rows.length === 12) {
    const missing = Array.from({ length: 12 }, (_, index) => index + 1).filter((month) => !seen.has(month));
    if (missing.length > 0) {
      errors.push(`缺少 ${missing.map((month) => periodCodeOf(params.fiscalYear, month)).join(' / ')} 的额度`);
    }
  }
  if (totalCents >= 0 && sumCents !== totalCents) {
    errors.push(
      `12 个月合计 ${formatCents(sumCents)} ≠ 年度额度 ${formatCents(totalCents)}（差额 ${formatCents(
        totalCents - sumCents,
      )}）：请把尾差记到 12 月`,
    );
  }
  return {
    ok: errors.length === 0,
    errors,
    sumCents,
    totalCents,
    diffCents: sumCents - totalCents,
  };
}

/**
 * 把差额一次性塞进 12 月（`尾差记 12 月` 的按钮行为）。
 *
 * 只在「其它 11 行已填、且差额让 12 月仍 ≥ 0」时有意义；否则返回 `null` 让调用方提示。
 */
export function applyTailToDecember(
  rows: ReadonlyArray<MonthDraftRow>,
  totalAmount: string | number,
): MonthDraftRow[] | null {
  if (rows.length !== 12) {
    return null;
  }
  const totalCents = toCents(totalAmount);
  const others = rows.filter((row) => row.month !== 12);
  const othersCents = sumMonthCents(others);
  const decemberCents = totalCents - othersCents;
  if (decemberCents < 0) {
    return null;
  }
  return rows.map((row) =>
    row.month === 12 ? { ...row, amount: centsToText(decemberCents) } : { ...row },
  );
}

/** 12 行 → 请求体的 `periods[]`（`month` + `period_code` 都给，后端二选一都能过） */
export function toPeriodReq(rows: ReadonlyArray<MonthDraftRow>): BudgetPeriodReq[] {
  return rows.map((row) => ({
    month: row.month,
    period_code: row.period_code,
    amount: centsToText(toCents(row.amount)),
  }));
}

/** 详情里的月度分解 → 12 行草稿（编辑 / 调整回填） */
export function monthRowsFromPeriods(
  fiscalYear: number,
  periods: ReadonlyArray<BudgetPeriodLine>,
): MonthDraftRow[] {
  const byMonth = new Map<number, BudgetPeriodLine>();
  for (const line of periods) {
    const month = Number(line.period_month);
    if (Number.isInteger(month) && month >= 1 && month <= 12) {
      byMonth.set(month, line);
    }
  }
  return emptyMonthRows(fiscalYear).map((row) => {
    const hit = byMonth.get(row.month);
    return hit ? { ...row, amount: centsToText(toCents(hit.amount)) } : row;
  });
}

/** 年度整组改期（新建对话框切年度时，12 行的 `period_code` 要跟着变） */
export function reyearRows(fiscalYear: number, rows: ReadonlyArray<MonthDraftRow>): MonthDraftRow[] {
  return rows.map((row) => ({ ...row, period_code: periodCodeOf(fiscalYear, row.month) }));
}

/** 年度下拉候选：今年 ±5 年（预算按自然年编制，够用且不刷屏） */
export function fiscalYearOptions(now: number = new Date().getFullYear(), span = 5): number[] {
  const years: number[] = [];
  for (let year = now - span; year <= now + 2; year += 1) {
    years.push(year);
  }
  return years.reverse();
}

// ==================== 列表筛选 / 请求体 ====================

/** 编制列表的筛选状态（`''` / `null` = 不过滤，与后端 `Option<T>` 同构） */
export interface BudgetFilterState {
  /** 年度（`null` = 全部年度） */
  fiscal_year: number | null;
  /** 部门 id（`''` = 全部） */
  dept_id: string;
  /** 状态（`''` = 全部） */
  status: string;
  /** 编制 / 调整（`''` = 全部） */
  change_type: string;
  /** 单号 / 科目 / 部门模糊 */
  keyword: string;
}

/** 空筛选（年度默认今年：预算是年度事务，很少有人跨年混看） */
export function emptyBudgetFilter(year: number = new Date().getFullYear()): BudgetFilterState {
  return { fiscal_year: year, dept_id: '', status: '', change_type: '', keyword: '' };
}

/** 筛选状态 → 请求参数（空串整键不发；`asParams` 再兜一次） */
export function budgetFilterParams(
  filter: BudgetFilterState,
  paging: { page: number; page_size: number },
): BudgetFilterQuery {
  return {
    page: paging.page,
    page_size: paging.page_size,
    fiscal_year: filter.fiscal_year === null ? undefined : filter.fiscal_year,
    dept_id: filter.dept_id.trim() || undefined,
    status: filter.status.trim() || undefined,
    change_type: filter.change_type.trim() || undefined,
    keyword: filter.keyword.trim() || undefined,
  };
}

/** 新建 / 编辑对话框的表单状态 */
export interface BudgetFormState {
  /** 年度 */
  fiscal_year: number;
  /** 部门 id */
  dept_id: string;
  /** 预算科目 id */
  account_id: string;
  /** 年度额度（文本） */
  total_amount: string;
  /** 控制策略（`INHERIT` = 继承） */
  control_mode: 'INHERIT' | 'OFF' | 'WARN' | 'BLOCK';
  /** 备注 */
  remark: string;
  /** 12 个月 */
  rows: MonthDraftRow[];
}

/** 空白表单（新建） */
export function emptyBudgetForm(fiscalYear: number = new Date().getFullYear()): BudgetFormState {
  return {
    fiscal_year: fiscalYear,
    dept_id: '',
    account_id: '',
    total_amount: '',
    control_mode: 'INHERIT',
    remark: '',
    rows: emptyMonthRows(fiscalYear),
  };
}

/** 表单 → 新建请求体 */
export function toCreatePayload(form: BudgetFormState): CreateBudgetReq {
  return {
    fiscal_year: form.fiscal_year,
    dept_id: form.dept_id,
    account_id: form.account_id,
    total_amount: centsToText(toCents(form.total_amount)),
    control_mode: toControlModePayload(form.control_mode),
    periods: toPeriodReq(form.rows),
    remark: form.remark.trim() || null,
  };
}

/** 表单 → 改草稿请求体（部门 / 科目 / 年度不可改，后端也没有这两个字段） */
export function toUpdatePayload(form: BudgetFormState): UpdateBudgetReq {
  return {
    total_amount: centsToText(toCents(form.total_amount)),
    control_mode: toControlModePayload(form.control_mode),
    periods: toPeriodReq(form.rows),
    remark: form.remark.trim() || null,
  };
}

/**
 * 表单 → 调整请求体（**原因必填**，后端 `validate_adjust` 空原因直接 400）。
 *
 * `change_reason` 长度上限 500（`budget_dto::MAX_REASON_LEN`），这里前置拦一次。
 */
export function toAdjustPayload(
  form: BudgetFormState,
  changeReason: string,
): AdjustBudgetReq {
  return {
    total_amount: centsToText(toCents(form.total_amount)),
    control_mode: toControlModePayload(form.control_mode),
    periods: toPeriodReq(form.rows),
    change_reason: changeReason.trim(),
    remark: form.remark.trim() || null,
  };
}

/** 调整原因上限（后端 `MAX_REASON_LEN = 500`） */
export const MAX_CHANGE_REASON = 500;

/** 备注上限（后端 `MAX_REMARK_LEN = 2000`） */
export const MAX_REMARK = 2000;

/** 表单前置校验（提交前的最后一道），返回逐条中文错误 */
export function checkBudgetForm(
  form: BudgetFormState,
  mode: 'create' | 'update' | 'adjust',
  changeReason = '',
): string[] {
  const errors: string[] = [];
  if (mode === 'create') {
    if (!form.dept_id.trim()) {
      errors.push('请选择部门');
    }
    if (!form.account_id.trim()) {
      errors.push('请选择预算科目（只能是末级科目）');
    }
  }
  const monthCheck = checkMonthDraft({
    fiscalYear: form.fiscal_year,
    totalAmount: form.total_amount,
    rows: form.rows,
  });
  errors.push(...monthCheck.errors);
  if (form.remark.length > MAX_REMARK) {
    errors.push(`备注超过 ${MAX_REMARK} 字（当前 ${form.remark.length} 字）`);
  }
  if (mode === 'adjust') {
    const reason = changeReason.trim();
    if (!reason) {
      errors.push('预算调整必须填写调整原因（审计要能说清为什么改额度）');
    } else if (reason.length > MAX_CHANGE_REASON) {
      errors.push(`调整原因超过 ${MAX_CHANGE_REASON} 字（当前 ${reason.length} 字）`);
    }
  }
  return errors;
}

// ==================== 行内动作（按状态 + 权限，不猜） ====================

/** 行内动作 key */
export type BudgetActionKey = 'detail' | 'edit' | 'submit' | 'adjust' | 'delete';

/** 一个行内动作 */
export interface BudgetAction {
  /** 动作 key */
  key: BudgetActionKey;
  /** 按钮文案 */
  label: string;
  /** 是否危险动作（红色 / 需二次确认） */
  danger: boolean;
}

/** 某状态是否可以改（只有草稿：`BUDGET_LOCKED` 把其余 4 个状态全锁了） */
export function canEditBudget(row: Pick<BudgetRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/** 某状态是否可以提交（只有草稿：`DRAFT → SUBMITTED` 是唯一入口） */
export function canSubmitBudget(row: Pick<BudgetRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/** 某状态是否可以删（只有草稿：后端对其它状态一律 409） */
export function canDeleteBudget(row: Pick<BudgetRow, 'status'>): boolean {
  return row.status === 'DRAFT';
}

/**
 * 某状态是否可以调整（只有 `ACTIVE`）。
 *
 * 为什么不给 `SUBMITTED`：调整会建新版本并立刻送审，同槽已有在审版本时后端
 * 会判重（`budget_handler::adjust` 的在审调整检查）。等它批完再调。
 */
export function canAdjustBudget(row: Pick<BudgetRow, 'status'>): boolean {
  return row.status === 'ACTIVE';
}

/**
 * 行内动作清单（顺序固定：详情 → 编辑 → 提交 → 调整 → 删除）。
 *
 * 权限只做「减法」：没权限的动作直接不出现，而不是置灰 —— 置灰的按钮会让人反复点。
 * 例外是**提交**：草稿但没提交权限时，用户最想知道「为什么我不能提交」，
 * 由页面在按钮旁给出 `t-tooltip`（见 `index.vue`），逻辑层只负责给清单。
 */
export function budgetRowActions(
  row: Pick<BudgetRow, 'status'>,
  permissions: BudgetPermissions,
): BudgetAction[] {
  const actions: BudgetAction[] = [{ key: 'detail', label: '详情', danger: false }];
  if (permissions.update && canEditBudget(row)) {
    actions.push({ key: 'edit', label: '编辑', danger: false });
  }
  if (permissions.submit && canSubmitBudget(row)) {
    actions.push({ key: 'submit', label: '提交', danger: false });
  }
  if (permissions.adjust && canAdjustBudget(row)) {
    actions.push({ key: 'adjust', label: '调整', danger: false });
  }
  if (permissions.delete && canDeleteBudget(row)) {
    actions.push({ key: 'delete', label: '删除', danger: true });
  }
  return actions;
}

/** 状态是否「不可再动」（详情抽屉里的只读提示用） */
export function isBudgetLocked(row: Pick<BudgetRow, 'status'>): boolean {
  return ['SUBMITTED', 'ACTIVE', 'SUPERSEDED', 'CLOSED', 'CANCELLED'].includes(row.status);
}

// ==================== 详情 / 报表展示 ====================

/** 一条占用记录的标签主题 */
export function usageStatusTheme(status: string | null | undefined): BudgetTagTheme {
  switch (status) {
    case 'OCCUPIED':
      return 'primary';
    case 'CONSUMED':
      return 'success';
    case 'RELEASED':
      return 'default';
    default:
      return 'default';
  }
}

/** 月度是否超支（`available < 0`，后端已算好） */
export function isMonthOver(month: { available: string | number }): boolean {
  return toCents(month.available) < 0;
}

/** 执行率文本：后端给的是百分比（`"85.25"`），补 `%`；空值是 `—` */
export function ratioText(value: string | number | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  return `${toNumber(value).toFixed(2)}%`;
}

/** 报表汇总（页面顶部 5 个数字） */
export interface ReportSummary {
  /** 行数（= 多少个 部门×科目 槽） */
  count: number;
  /** 额度合计（分） */
  totalCents: number;
  /** 占用中合计 */
  occupiedCents: number;
  /** 已执行合计 */
  consumedCents: number;
  /** 已用合计 */
  usedCents: number;
  /** 可用合计（可为负） */
  availableCents: number;
  /** 全公司执行率（%） */
  ratio: number;
  /** 超支槽数 */
  overCount: number;
}

/**
 * 汇总报表行。
 *
 * 为什么前端自己加：后端报表是**逐槽**返回的（部门 × 科目），接口没有合计行
 * （`budget_handler::budget_report` 只 `map` 了 `ReportRes`）。合计必须用整数分加，
 * 12 个槽各带 4 位小数时浮点累加会出现 `0.0000000001` 这种尾巴。
 *
 * 「执行率」= 已用 / 额度（额度 0 时按 0 处理，不报 Infinity）。
 */
export function summarizeReport(rows: ReadonlyArray<BudgetReportRow>): ReportSummary {
  let totalCents = 0;
  let occupiedCents = 0;
  let consumedCents = 0;
  let availableCents = 0;
  let overCount = 0;
  for (const row of rows) {
    totalCents += toCents(row.total_amount);
    occupiedCents += toCents(row.occupied);
    consumedCents += toCents(row.consumed);
    availableCents += toCents(row.available);
    if (row.over) {
      overCount += 1;
    }
  }
  const usedCents = occupiedCents + consumedCents;
  const ratio = totalCents === 0 ? 0 : Math.round((usedCents / totalCents) * 10000) / 100;
  return { count: rows.length, totalCents, occupiedCents, consumedCents, usedCents, availableCents, ratio, overCount };
}

/** 报表行 → 可展开的分月明细（`t-table` 的 `expanded-row` 用） */
export function reportMonths(row: BudgetReportRow): Array<{ period_code: string; amount: string; used: string; available: string; over: boolean }> {
  return (row.months ?? []).map((month) => ({
    period_code: month.period_code,
    amount: month.amount,
    used: month.used,
    available: month.available,
    over: toCents(month.available) < 0,
  }));
}

/** 报表是否「一条都没有」（区分「没数据」和「没筛到」） */
export function reportEmptyText(fiscalYear: number | null): string {
  return fiscalYear ? `${fiscalYear} 年还没有生效（ACTIVE）的预算` : '还没有生效（ACTIVE）的预算';
}

// ==================== 科目映射 ====================

/** 映射类型下拉（中文文案是运营口径，`value` 逐字抄后端） */
export const MAP_TYPE_OPTIONS: ReadonlyArray<{ label: string; value: BudgetMapType; hint: string }> = [
  { label: '物料', value: 'MATERIAL', hint: '按单个物料指定预算科目（最精细）' },
  { label: '物料类型', value: 'MATERIAL_TYPE', hint: '按物料分类兜底（同一类型的所有物料）' },
  { label: '仓库', value: 'WAREHOUSE', hint: '按仓库指定（与物料同期命中时比优先级）' },
  { label: '默认', value: 'DEFAULT', hint: '全公司兜底：一条都没有时才轮到它' },
];

/** 映射类型 → 中文 */
export function mapTypeLabel(value: string | null | undefined): string {
  const hit = MAP_TYPE_OPTIONS.find((item) => item.value === value);
  return hit ? hit.label : String(value ?? '—');
}

/** 映射类型 → 中文 + 精细度说明（对话框里给运营看的） */
export function mapTypeHint(value: string | null | undefined): string {
  const hit = MAP_TYPE_OPTIONS.find((item) => item.value === value);
  return hit ? hit.hint : '';
}

/**
 * 映射的「目标」展示文本。
 *
 * - `DEFAULT` 没有目标（全公司兜底），显示「全公司兜底」；
 * - 其余类型优先 `ref_code`（编码比 UUID 好认），没有编码才退回短 id。
 */
export function mappingTargetText(row: Pick<BudgetMappingRow, 'map_type' | 'ref_code' | 'ref_id'>): string {
  if (row.map_type === 'DEFAULT') {
    return '全公司兜底';
  }
  const code = String(row.ref_code ?? '').trim();
  if (code) {
    return code;
  }
  const id = String(row.ref_id ?? '').trim();
  return id ? `${id.slice(0, 8)}…` : '—';
}

/** 映射表单草稿 */
export interface MappingDraft {
  /** 类型 */
  map_type: BudgetMapType;
  /** 目标 id（MATERIAL = 物料 id / WAREHOUSE = 仓库 id） */
  ref_id: string;
  /** 目标编码（MATERIAL = 物料编码 / MATERIAL_TYPE = 物料类型编码） */
  ref_code: string;
  /** 预算科目 id */
  account_id: string;
  /** 部门覆盖（`''` = 全公司） */
  dept_id: string;
  /** 优先级 */
  priority: number;
  /** 1 启用 / 0 停用 */
  status: number;
  /** 备注 */
  remark: string;
}

/** 空白映射草稿（默认 DEFAULT —— 最常见的配置就是「先兜个底」） */
export function emptyMappingDraft(): MappingDraft {
  return {
    map_type: 'DEFAULT',
    ref_id: '',
    ref_code: '',
    account_id: '',
    dept_id: '',
    priority: 0,
    status: 1,
    remark: '',
  };
}

/** 映射行 → 草稿（编辑回填） */
export function draftFromMappingRow(row: BudgetMappingRow): MappingDraft {
  return {
    map_type: row.map_type,
    ref_id: row.ref_id ?? '',
    ref_code: row.ref_code ?? '',
    account_id: row.account_id,
    dept_id: row.dept_id ?? '',
    priority: row.priority,
    status: row.status,
    remark: row.remark ?? '',
  };
}

/**
 * 映射表单校验（镜像 `budget_dto::validate_mapping` + 前端必填项）。
 *
 * 三种目标形态各一套必填规则（与后端 `validate_mapping` 逐条对应，见
 * `mapping.vue` 的注释与 `MappingDraft`）：
 *
 * - `DEFAULT`：不许有 `ref_id` / `ref_code`（全公司兜底，同一部门只能一条）；
 * - `MATERIAL_TYPE`：只填 `ref_code`（物料类型编码），`ref_id` 必须留空；
 * - `MATERIAL` / `WAREHOUSE`：`ref_id` 必填，`ref_code` 只做展示与对账。
 */
export function checkMappingDraft(draft: MappingDraft): string[] {
  const errors: string[] = [];
  if (!draft.account_id.trim()) {
    errors.push('请选择预算科目（必须是末级科目）');
  }
  const isDefault = draft.map_type === 'DEFAULT';
  const isMaterialType = draft.map_type === 'MATERIAL_TYPE';
  if (isDefault && draft.ref_id.trim()) {
    errors.push('DEFAULT 映射不许给 ref_id（它是全公司兜底，不指向任何目标）');
  }
  if (isDefault && draft.ref_code.trim()) {
    errors.push('DEFAULT 映射不许给 ref_code（全公司兜底不挂任何目标编码）');
  }
  if (isMaterialType) {
    if (!draft.ref_code.trim()) {
      errors.push('物料类型映射必须选一个物料类型（解析按这个编码命中）');
    }
    if (draft.ref_id.trim()) {
      errors.push('物料类型映射不能给 ref_id（物料类型是编码枚举，没有独立 id）');
    }
  }
  if (!isDefault && !isMaterialType) {
    if (!draft.ref_id.trim()) {
      errors.push(`${mapTypeLabel(draft.map_type)} 映射必须选一个目标`);
    }
    if (!draft.ref_code.trim()) {
      errors.push(`${mapTypeLabel(draft.map_type)} 映射缺少目标编码（展示与对账都靠它）`);
    }
  }
  if (![0, 1].includes(draft.status)) {
    errors.push(`状态 ${draft.status} 非法（只能是 0 停用 / 1 启用）`);
  }
  if (!Number.isInteger(draft.priority)) {
    errors.push('优先级必须是整数');
  }
  return errors;
}

/**
 * 映射草稿 → 请求体。
 *
 * `DEFAULT` 与 `MATERIAL_TYPE` 的 `ref_id` 一律 `null`：前者是全公司兜底，
 * 后者的目标只有类型编码（后端对这两种形态都会再把 `ref_id` 归一成 NULL，
 * 前端这里先发干净，避免联调时看着像「多传了一个字段」）。
 */
export function toMappingReq(draft: MappingDraft): BudgetMappingReq {
  const isDefault = draft.map_type === 'DEFAULT';
  const noRefId = isDefault || draft.map_type === 'MATERIAL_TYPE';
  return {
    map_type: draft.map_type,
    ref_id: noRefId ? null : draft.ref_id.trim() || null,
    ref_code: isDefault ? null : draft.ref_code.trim() || null,
    account_id: draft.account_id.trim(),
    dept_id: draft.dept_id.trim() || null,
    priority: draft.priority,
    status: draft.status,
    remark: draft.remark.trim() || null,
  };
}

/** 映射列表筛选状态 */
export interface MappingFilterState {
  /** 类型（`''` = 全部） */
  map_type: string;
  /** 部门覆盖（`''` = 全部） */
  dept_id: string;
}

/** 空映射筛选 */
export function emptyMappingFilter(): MappingFilterState {
  return { map_type: '', dept_id: '' };
}

/**
 * 映射筛选 → 请求参数。
 *
 * ⚠️ 类型走 **`status`** 这个键（`budget_model::map_page` 从 `q.status` 读 `map_type`）。
 * `fetchBudgetMappingPage` 里也做了同样的翻译，两处一致即可；这里单独给纯逻辑是为了
 * 单测能钉住「参数名不许改回 map_type」。
 */
export function mappingFilterParams(
  filter: MappingFilterState,
  paging: { page: number; page_size: number },
): { page: number; page_size: number; status?: string; dept_id?: string } {
  return {
    page: paging.page,
    page_size: paging.page_size,
    status: filter.map_type.trim() || undefined,
    dept_id: filter.dept_id.trim() || undefined,
  };
}

/** 映射是否启用（`t-tag` 文案） */
export function mappingStatusLabel(status: number): string {
  return status === 1 ? '启用' : '停用';
}

/** 映射启停主题 */
export function mappingStatusTheme(status: number): BudgetTagTheme {
  return status === 1 ? 'success' : 'default';
}

// ==================== 与 n08 的镜像（前端侧自检） ====================

/**
 * 预算三页的路由 path ↔ 视图文件（与 `sys_menu` V0227 种子逐字一致）。
 *
 * n08 从库里读菜单 path 再比对 `router/index.ts`；这份常量让**前端单测**能反着钉住
 * 「菜单上有的三条 path，路由和视图文件都真的在」—— 省得哪天有人删了路由以为菜单还能点。
 */
export const BUDGET_ROUTES: ReadonlyArray<{ path: string; view: string }> = [
  { path: '/business/erp/budget', view: 'views/business/erp/budget/index.vue' },
  { path: '/business/erp/budget/mapping', view: 'views/business/erp/budget/mapping.vue' },
  { path: '/business/erp/budget/report', view: 'views/business/erp/budget/report.vue' },
];

/** 详情里的「年度已用占比」（详情抽屉顶部进度条） */
export function detailUsagePercent(detail: Pick<BudgetDetail, 'budget' | 'year_used'>): number {
  const total = toCents(detail.budget.total_amount);
  if (total <= 0) {
    return 0;
  }
  const used = toCents(detail.year_used);
  return Math.min(100, Math.round((used / total) * 10000) / 100);
}

/** 占用记录里「超预算」的快照说明（后端已冻结当时的超支额） */
export function usageOverText(usage: Pick<BudgetUsageRow, 'over_budget' | 'over_amount'>): string {
  if (!usage.over_budget) {
    return '';
  }
  return `提交时超支 ${formatCents(toCents(usage.over_amount ?? '0'))}`;
}
