/**
 * 批次成本页纯逻辑（M5-07 · 分批法：归集表 / 来源 / 结转 / 在制余额 / 台账）。
 *
 * 只放「输入 → 输出」的判定与文案：入参拼装、金额累加（整数分）、数量 / 百分比裁剪、
 * 三态与主题映射、拦下原因文案、台账自洽校验。`.vue` 只负责画和串请求。
 *
 * # 为什么金额一律走整数分
 *
 * 后端出参是字符串（`"1200.0000"`），前端是 IEEE754 double。`0.1 + 0.2 !== 0.3`，
 * 所以**显示前**先过 `toCents`（`views/business/erp/shared/money`）。本文件不重复实现
 * 舍入，只把「哪几个字段相加」这件事集中在一处 —— 这正是各页面最容易抄歪的地方
 * （归集表是 `mat/labor/oh`，在制是 `material/labor/overhead`，混一次就把材料记成制造费用）。
 *
 * # 两条口径（后端逐字同源）
 *
 * 1. **在制余额不夹零**：负数说明「转出去的比归集进来的多」（来源被作废 / 有人改库），
 *    夹成 0 会把异常藏起来 —— 页面照原样显示负数并挂警告，不减人不加分；
 * 2. **结转的「可转」判据只有两条**：闸门为空 **且** 本次待结转金额非零。
 *    前端先按这两条置灰按钮并把原因写在按钮旁边，不让用户点下去才吃 409。
 */

import type {
  AmountsRes,
  BatchCollect,
  BatchFinishPreview,
  BatchSources,
  CollectSheetQuery,
  CostBatchElement,
  CostBatchStatus,
  ElementSources,
  SheetAmounts,
  SheetRow,
  SheetSort,
  SourceState,
  WipAmounts,
  WipBalanceQuery,
  WipLedger,
  WipRow,
  WipSort,
} from '@/api/business/erp/cost-batch-kinds';
import {
  COST_BATCH_ELEMENT_LABELS,
  COST_BATCH_STATUS_LABELS,
  SOURCE_STATE_LABELS,
} from '@/api/business/erp/cost-batch-kinds';
import { formatCents, formatMoney, isMonthKey, toCents, todayPeriod } from '@/views/business/erp/shared/money';

// ==================== 期间 ====================

/** 默认期间 = 北京时间本月（与后端 `today_cst()` 同口径；服务端不会替我们猜）。 */
export function defaultPeriod(now: Date = new Date()): string {
  return todayPeriod(now);
}

/**
 * 期间兜底：合法 `yyyy-MM` 原样返回，否则回落本月。
 *
 * 为什么不在页面里写 `filter.period_code || todayPeriod()`：空串是「用户清空了」，
 * 而不是「用今天」—— 清空后必须回到本月再请求，否则列表接口会 400（期间必填）。
 */
export function periodOrFallback(value: string | null | undefined, now: Date = new Date()): string {
  const text = (value ?? '').trim();
  return isMonthKey(text) ? text : defaultPeriod(now);
}

// ==================== 数量 / 百分比 / 金额 ====================

/**
 * 数量串裁剪：去掉小数尾零（`"12.0000"` → `"12"`、`"1.5000"` → `"1.5"`）。
 *
 * 数量是 `NUMERIC(18,4)`，直接展示会是 `12.0000` 这种噪声；数量**不做千分位**
 * （`1000` 件的批次写成 `1,000` 会让人以为是金额）。
 */
export function trimDecimal(value: string | null | undefined): string {
  const text = (value ?? '').trim();
  if (text === '') {
    return '0';
  }
  if (!text.includes('.')) {
    return text;
  }
  const trimmed = text.replace(/0+$/, '').replace(/\.$/, '');
  return trimmed === '' || trimmed === '-' || trimmed === '-0' ? '0' : trimmed;
}

/** 完工率文案：`66.67%`（后端给的是百分数串，不再乘 100）。 */
export function pctText(value: string | null | undefined): string {
  const text = (value ?? '').trim();
  if (text === '') {
    return '—';
  }
  return `${trimDecimal(text)}%`;
}

/** 归集口径金额合计（分）：`mat + labor + oh`。 */
export function sheetTotalCents(amounts: SheetAmounts): number {
  return toCents(amounts.mat) + toCents(amounts.labor) + toCents(amounts.oh);
}

/** 在制 / 结转口径金额合计（分）：`material + labor + overhead`。 */
export function wipTotalCents(amounts: WipAmounts | AmountsRes): number {
  return toCents(amounts.material) + toCents(amounts.labor) + toCents(amounts.overhead);
}

/** 归集口径三要素是否全 0。 */
export function sheetAmountsEmpty(amounts: SheetAmounts): boolean {
  return (
    toCents(amounts.mat) === 0 && toCents(amounts.labor) === 0 && toCents(amounts.oh) === 0
  );
}

/** 在制口径三要素是否全 0。 */
export function wipAmountsEmpty(amounts: WipAmounts | AmountsRes): boolean {
  return (
    toCents(amounts.material) === 0 &&
    toCents(amounts.labor) === 0 &&
    toCents(amounts.overhead) === 0
  );
}

/** 在制口径是否有任一要素为负（余额为负 = 账要查，见模块头口径①）。 */
export function wipAmountsNegative(amounts: WipAmounts | AmountsRes): boolean {
  return (
    toCents(amounts.material) < 0 || toCents(amounts.labor) < 0 || toCents(amounts.overhead) < 0
  );
}

/** 金额文案（分 → `1,200.00`），页面模板里统一用它。 */
export function money(value: string | number | null | undefined): string {
  return formatMoney(value);
}

/**
 * 单位成本文案：保留最多 6 位小数。
 *
 * 为什么不能直接用 `money`：`formatMoney` 固定 2 位，单位成本 `0.003750` 会被显示成
 * `0.00` —— 一张 0.0037 元/件的成本表看着像免费。单位成本是「本次结转 / 完工数量」
 * 且后端就是 6 位标度，这里原样保留（只去尾零、不加千分位）。
 */
export function unitCostText(value: string | null | undefined): string {
  const text = (value ?? '').trim();
  return text === '' ? '—' : trimDecimal(text);
}

// ==================== 表格行拍平（列要扁的） ====================

/** 归集表一行 + 拍平后的金额列（`t-table` 的列只能取扁平字段）。 */
export interface SheetRowView extends SheetRow {
  /**
   * 合计金额（三项相加）。
   *
   * 为什么不用后端给的 `total` 直接用：两者**必须相等**，页面自己算一遍是把
   * 「后端合计口径漂了」变成看得见的差；列里取这一列，不取裸的 `total`。
   */
  total_amount: string;
  /** 完工率文案。 */
  pct_text: string;
  /** 计划数量（裁剪）。 */
  plan_qty_text: string;
  /** 完工数量（裁剪）。 */
  completed_qty_text: string;
  /** 在制数量（裁剪）。 */
  wip_qty_text: string;
}

/** 归集表行拍平（金额字段后端已是扁的，这里只补算合计与文案）。 */
export function flattenSheetRow(row: SheetRow): SheetRowView {
  return {
    ...row,
    total_amount: formatCents(toCents(row.mat) + toCents(row.labor) + toCents(row.oh)),
    pct_text: pctText(row.completion_pct),
    plan_qty_text: trimDecimal(row.plan_qty),
    completed_qty_text: trimDecimal(row.completed_qty),
    wip_qty_text: trimDecimal(row.wip_qty),
  };
}

/** 在制余额一行 + 拍平后的金额列。 */
export interface WipRowView extends WipRow {
  /** 累计归集合计。 */
  collected_total: string;
  /** 已结转合计。 */
  transferred_total: string;
  /** 在制余额合计。 */
  balance_total: string;
  /** 余额 · 材料。 */
  balance_mat: string;
  /** 余额 · 人工。 */
  balance_labor: string;
  /** 余额 · 制造费用。 */
  balance_oh: string;
  /** 本期投入合计。 */
  period_in_total: string;
  /** 本期转出合计。 */
  period_out_total: string;
  /** 在制数量（裁剪）。 */
  wip_qty_text: string;
  /** 完工率文案。 */
  pct_text: string;
  /** 余额为负（挂红字）。 */
  negative: boolean;
  /** 期间跨度文案：`2026-10 ~ 2026-11`。 */
  period_span: string;
}

/** 在制余额行拍平。 */
export function flattenWipRow(row: WipRow): WipRowView {
  return {
    ...row,
    collected_total: formatCents(wipTotalCents(row.collected)),
    transferred_total: formatCents(wipTotalCents(row.transferred)),
    balance_total: formatCents(wipTotalCents(row.balance)),
    balance_mat: formatCents(toCents(row.balance.material)),
    balance_labor: formatCents(toCents(row.balance.labor)),
    balance_oh: formatCents(toCents(row.balance.overhead)),
    period_in_total: formatCents(wipTotalCents(row.period_in)),
    period_out_total: formatCents(wipTotalCents(row.period_out)),
    wip_qty_text: trimDecimal(row.wip_qty),
    pct_text: pctText(row.completion_pct),
    negative: wipAmountsNegative(row.balance),
    period_span: periodSpan(row.first_period, row.last_period),
  };
}

/** 期间跨度：两端都有才是区间，只有一头就显示一头，都没有给 `—`。 */
export function periodSpan(first: string | null, last: string | null): string {
  if (first && last && first !== last) {
    return `${first} ~ ${last}`;
  }
  return first || last || '—';
}

// ==================== 筛选状态 → 查询入参 ====================

/** 归集表筛选状态（页面的 `reactive` 形状）。 */
export interface SheetFilterState {
  /** 期间 `yyyy-MM`。 */
  period_code: string;
  /** 关键字。 */
  keyword: string;
  /** 状态（`''` / `ALL` = 不限）。 */
  status: CostBatchStatus | '';
  /** 车间部门 id。 */
  workshop_dept_id: string;
  /** 排序键。 */
  sort: SheetSort;
}

/** 归集表默认筛选（期间由页面在 `onMounted` 里补上月 / 记忆值）。 */
export function emptySheetFilter(): SheetFilterState {
  return {
    period_code: '',
    keyword: '',
    status: '',
    workshop_dept_id: '',
    sort: 'total',
  };
}

/**
 * 归集表入参拼装。
 *
 * 口径：`ALL` 与空串都**整键缺席**（后端把 `ALL` 归一成 `None`，前端少发一个键更省事）；
 * `period_code` 永远发（必填，且保持 `yyyy-MM` 形状）。期间为空 / 被清空时回落本月 ——
 * 这条兜底不是「猜月份」而是**防 400**：`ListPage` 的「重置」会把 `custom` 筛选清成
 * `undefined`（`clearFilter` 的既有语义），直接发出去后端必回「period_code 必填」。
 */
export function buildSheetQuery(
  state: SheetFilterState,
  page: number,
  pageSize: number,
): CollectSheetQuery {
  const status = state.status && state.status !== 'ALL' ? state.status : undefined;
  const keyword = state.keyword.trim();
  return {
    period_code: periodOrFallback(state.period_code),
    ...(keyword ? { keyword } : {}),
    ...(status ? { status } : {}),
    ...(state.workshop_dept_id ? { workshop_dept_id: state.workshop_dept_id } : {}),
    sort: state.sort || 'total',
    page,
    page_size: pageSize,
  };
}

/** 在制余额筛选状态。 */
export interface WipFilterState {
  /** 期间。 */
  period_code: string;
  /** 关键字。 */
  keyword: string;
  /** 状态。 */
  status: CostBatchStatus | '';
  /** 车间部门 id。 */
  workshop_dept_id: string;
  /** 只看有余额（下拉值：`'1'` / `''`）。 */
  only_wip: string;
  /** 排序键。 */
  sort: WipSort;
}

/** 在制余额默认筛选。 */
export function emptyWipFilter(): WipFilterState {
  return {
    period_code: '',
    keyword: '',
    status: '',
    workshop_dept_id: '',
    only_wip: '',
    sort: 'balance',
  };
}

/**
 * 「只看有余额」下拉值 → 后端布尔。
 *
 * 用下拉（`'1'` / `''`）而不是 `t-switch`：`ListPage` 的 `searchOnChange` 只对
 * `select` 生效，开关得再点一次「查询」，用户会以为开关坏了。`false` 整键不发
 * （后端默认 `only_wip = false`）。
 */
export function onlyWipFlag(value: string): boolean | undefined {
  return value === '1' ? true : undefined;
}

/** 在制余额入参拼装（口径同 {@link buildSheetQuery}）。 */
export function buildWipQuery(
  state: WipFilterState,
  page: number,
  pageSize: number,
): WipBalanceQuery {
  const status = state.status && state.status !== 'ALL' ? state.status : undefined;
  const keyword = state.keyword.trim();
  const onlyWip = onlyWipFlag(state.only_wip);
  return {
    period_code: periodOrFallback(state.period_code),
    ...(keyword ? { keyword } : {}),
    ...(status ? { status } : {}),
    ...(state.workshop_dept_id ? { workshop_dept_id: state.workshop_dept_id } : {}),
    ...(onlyWip ? { only_wip: onlyWip } : {}),
    sort: state.sort || 'balance',
    page,
    page_size: pageSize,
  };
}

// ==================== 词表 / 主题 ====================

/** 批次状态中文（未知值原样返回，不吞）。 */
export function statusLabel(status: string): string {
  return COST_BATCH_STATUS_LABELS[status as CostBatchStatus] ?? status;
}

/** 批次状态标签配色：在产=进行中、已关批=完成、已作废=置灰。 */
export function statusTheme(status: string): 'primary' | 'success' | 'default' | 'warning' {
  if (status === 'OPEN') {
    return 'primary';
  }
  if (status === 'CLOSED') {
    return 'success';
  }
  if (status === 'CANCELLED') {
    return 'default';
  }
  return 'warning';
}

/** 要素中文。 */
export function elementLabel(element: string): string {
  return COST_BATCH_ELEMENT_LABELS[element as CostBatchElement] ?? element;
}

/** 来源三态中文（后端也直出 `state_label`，这里只做兜底）。 */
export function sourceStateLabel(state: string): string {
  return SOURCE_STATE_LABELS[state as SourceState] ?? state;
}

/** 来源三态配色：已归集=绿、待归集=黄（本次会写）、不可归集=灰（不写）。 */
export function sourceStateTheme(state: string): 'success' | 'warning' | 'default' {
  if (state === 'COLLECTED') {
    return 'success';
  }
  if (state === 'PENDING') {
    return 'warning';
  }
  return 'default';
}

/**
 * 警告级别配色（`WARN` 黄、`INFO` 蓝）。
 *
 * 蓝用 `primary`：`t-tag` 的 theme 值域里没有 `info`（`default|primary|warning|danger|success`），
 * 传 `info` 会被 TS 判红、运行时也不上色。
 */
export function warningTheme(level: string): 'warning' | 'primary' {
  return level === 'WARN' ? 'warning' : 'primary';
}

/** 按要素取那一路来源（页面三个卡片共用一份取数）。 */
export function elementSourcesOf(
  sources: BatchSources | null,
  element: CostBatchElement,
): ElementSources | null {
  if (!sources) {
    return null;
  }
  if (element === 'MATERIAL') {
    return sources.material;
  }
  if (element === 'LABOR') {
    return sources.labor;
  }
  return sources.overhead;
}

// ==================== 三路来源的文案 ====================

/**
 * 一路要素的一句话摘要（页面卡片头部直接展示）。
 *
 * 形如：`已归集 3 行 · 1,200.00 ｜ 本次将写 2 行 · 800.00 ｜ 不可归集 1 行 ｜ 归集后 2,000.00`
 * 有「过期将清」时追加一段 —— 那一行的来源变了（金额改小 / 改挂 / 作废），
 * 归集后金额会**小于**已有金额，不解释会被当成丢数。
 */
export function elementSummaryText(element: ElementSources): string {
  const parts = [
    `已归集 ${element.collected_rows} 行 · ${money(element.collected_amount)}`,
    `本次将写 ${element.pending_rows} 行 · ${money(element.pending_amount)}`,
  ];
  if (element.purged_rows > 0) {
    parts.push(`过期将清 ${element.purged_rows} 行 · ${money(element.purged_amount)}`);
  }
  if (element.uncollected_rows > 0) {
    parts.push(`不可归集 ${element.uncollected_rows} 行`);
  }
  parts.push(`归集后 ${money(element.after_amount)}`);
  return parts.join(' ｜ ');
}

/** 归集回执文案：后端 `message` + 三段写入量（`inserted` / `purged` / `skipped`）。 */
export function collectResultText(result: BatchCollect): string {
  const legs = [result.material, result.labor, result.overhead].map((item) => {
    const bits = [`写 ${item.inserted} 行 · ${money(item.inserted_amount)}`];
    if (item.purged > 0) {
      bits.push(`清 ${item.purged} 行 · ${money(item.purged_amount)}`);
    }
    if (item.skipped_existing > 0) {
      bits.push(`跳过 ${item.skipped_existing} 行（已归集）`);
    }
    return `${item.element_label}：${bits.join('，')}`;
  });
  return [result.message, legs.join('；')].filter((text) => text.trim() !== '').join('\n');
}

// ==================== 结转文案与可转判定 ====================

/**
 * 能不能点「结转」：**闸门为空 且 有钱可转**（口径②）。
 *
 * 注意 `settled_in_books`（已有已结算计算单）后端已经折进 `blockers`，
 * 这里不再单独判一次 —— 两处判据并存时，迟早会出现「按钮可点但接口 409」。
 */
export function canFinish(preview: BatchFinishPreview | null): boolean {
  if (!preview) {
    return false;
  }
  return preview.blockers.length === 0 && preview.transferable;
}

/** 按钮置灰的原因（可转时返回空串）。 */
export function finishDisabledReason(preview: BatchFinishPreview | null): string {
  if (!preview) {
    return '结转试算未加载';
  }
  if (preview.blockers.length > 0) {
    return preview.blockers[0]?.message ?? '有前置未满足';
  }
  if (!preview.transferable) {
    return '本次可结转金额是 0（累计归集已全部转出，无需再转）';
  }
  return '';
}

/** 结转试算的一句话摘要：本次金额 / 完工数量 / 单位成本 / 第几次。 */
export function finishSummaryText(preview: BatchFinishPreview): string {
  return [
    `第 ${preview.transfer_no} 次结转`,
    `本次 ${money(preview.pending.total)}`,
    `完工数量 ${trimDecimal(preview.finished_qty)}`,
    `本次单位成本 ${unitCostText(preview.unit_cost)}`,
    `结转后累计单位成本 ${unitCostText(preview.cumulative_unit_cost)}`,
  ].join(' ｜ ');
}

/**
 * 闸门文案（列表展示用）：每条都带上「怎么修」。
 *
 * 只展示 `message` 会让人知道「不行」但不知道「怎么办」，而结转账的修法分散在
 * 领料单 / 工时台账 / 期间审批三处 —— 少了后半句，用户只能去问会计。
 */
export function blockerLines(preview: BatchFinishPreview | null): string[] {
  if (!preview) {
    return [];
  }
  return preview.blockers.map((item) => `${item.message}（怎么修：${item.fix}）`);
}

// ==================== 台账自洽校验 ====================

/** 台账逐期回放的展示行（列要扁的）。 */
export interface LedgerRowView {
  /** 期间。 */
  period_code: string;
  /** 期初合计。 */
  opening_total: string;
  /** 本期投入合计。 */
  inflow_total: string;
  /** 本期转出合计。 */
  outflow_total: string;
  /** 期末合计。 */
  closing_total: string;
  /** 本期归集行数。 */
  collect_lines: number;
  /** 本期结转次数。 */
  transfer_times: number;
}

/** 台账期间行拍平。 */
export function flattenLedgerRows(ledger: WipLedger | null): LedgerRowView[] {
  if (!ledger) {
    return [];
  }
  return ledger.periods.map((row) => ({
    period_code: row.period_code,
    opening_total: formatCents(wipTotalCents(row.opening)),
    inflow_total: formatCents(wipTotalCents(row.inflow)),
    outflow_total: formatCents(wipTotalCents(row.outflow)),
    closing_total: formatCents(wipTotalCents(row.closing)),
    collect_lines: row.collect_lines,
    transfer_times: row.transfer_times,
  }));
}

/**
 * 台账自洽校验：返回**不一致**的说明（空 = 自洽）。
 *
 * 三条判据（与后端单测 `closing_at_cutoff_equals_cumulative` 同口径）：
 *
 * 1. 每期 `期初 + 投入 − 转出 = 期末`；
 * 2. 下一期期初 = 上一期期末（滚动链不能断）；
 * 3. 最后一期期末 = 汇总的在制余额（累计式与滚动式必须是同一个数）。
 *
 * 页面把它渲染成一条红字提示：**数不对时先别信这张表**，而不是照单全收。
 */
export function ledgerMismatches(ledger: WipLedger | null): string[] {
  if (!ledger) {
    return [];
  }
  const issues: string[] = [];
  let previousClosing: number | null = null;
  for (const row of ledger.periods) {
    const opening = wipTotalCents(row.opening);
    const closing = wipTotalCents(row.closing);
    const expected = opening + wipTotalCents(row.inflow) - wipTotalCents(row.outflow);
    if (expected !== closing) {
      issues.push(
        `${row.period_code}：期初 + 投入 − 转出 = ${formatCents(expected)}，但期末是 ${formatCents(closing)}`,
      );
    }
    if (previousClosing !== null && opening !== previousClosing) {
      issues.push(
        `${row.period_code}：期初 ${formatCents(opening)} ≠ 上期期末 ${formatCents(previousClosing)}`,
      );
    }
    previousClosing = closing;
  }
  const summary = wipTotalCents(ledger.summary.balance);
  if (previousClosing !== null && previousClosing !== summary) {
    issues.push(
      `最后一期期末 ${formatCents(previousClosing)} ≠ 汇总在制余额 ${formatCents(summary)}`,
    );
  }
  return issues;
}

/** 台账的期初是否与第一期期初一致（截断时后端把更早的净流量折进期初，只做展示提示）。 */
export function ledgerOpeningText(ledger: WipLedger | null): string {
  if (!ledger) {
    return '—';
  }
  const head = `期初 ${money(ledger.opening.total)}`;
  return ledger.truncated ? `${head}（含更早期间的净额，明细已截断）` : head;
}
