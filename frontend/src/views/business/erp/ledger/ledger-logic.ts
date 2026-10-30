/**
 * 总账页纯逻辑（P2-E2）—— 流水余额校验 / 科目余额表合计与平衡判定 / 查询参数。
 *
 * # 两个 Tab 的口径差一寸都不能混
 *
 * ```text
 * Tab1 总账（流水）  GET /erp/ledger/page          单科目、逐行余额 running_balance（不含期初）
 * Tab2 科目余额表    GET /erp/ledger/trial-balance  全科目、期初/本期/期末 + Σ借 = Σ贷 校验
 * ```
 *
 * Tab1 的 `running_balance` 是**本页内**的净额累加（第一行 = 本页之前所有行净额 + 本行），
 * 所以翻页后第一行的余额不是从 0 开始 —— 界面上必须写清楚，否则会计会以为余额算错了。
 *
 * # ★ 平衡校验（硬红线：必须可视化）
 *
 * 余额表的每一行都必须满足恒等式：
 *
 * ```text
 * 期初净额（借 − 贷） + 本期借 − 本期贷 = 期末净额（借 − 贷）
 * ```
 *
 * 全体行再满足 `Σ本期借 = Σ本期贷` 与 `Σ期末借 = Σ期末贷`。
 * 后端不平会直接 500，所以正常响应必然平；前端**仍然自己加一遍**，
 * 因为「界面上写着平」这件事必须是本地能证明的，而不是信任一个布尔字段。
 *
 * # 金额一律整数分
 *
 * 见 `../shared/money.ts`。`opening_debit − opening_credit` 这种拆两侧的排版
 * 在浮点下会出现 `-0.00`，用分就不会。
 */
import type { AccountRow, LedgerRow, TrialBalanceRow } from '@/api/business/erp';
import { dash, isMonthKey, toCents, todayPeriod } from '../shared/money';

/** 每页条数（流水行数多，与其它 ERP 列表页同口径 20） */
export const LEDGER_PAGE_SIZE = 20;

// ==================== Tab1：流水 ====================

/** Tab1 筛选行 */
export interface LedgerFilterState {
  /** 科目 id（必填；下拉选中的是 id） */
  account_id: string;
  /** 期间起 `yyyy-MM` 或 `yyyy-MM-dd`（可空 = 不限） */
  period_from: string;
  /** 期间止 */
  period_to: string;
}

/** 空筛选（科目必选，所以默认空串 → 页面提示「请选择科目」而不是发请求） */
export function emptyLedgerFilter(): LedgerFilterState {
  return { account_id: '', period_from: '', period_to: '' };
}

/**
 * 筛选 → 流水查询参数。
 *
 * ⚠️ `account_id` 与 `account_code` 后端**必须给一个**（都不给 → 400 + 10001），
 * 所以这里没选科目就返回 `null`，调用方据此拦住请求并提示 —— 与其发一个必然 400 的
 * 请求，不如在界面上说清楚。
 */
export function buildLedgerQuery(
  filter: LedgerFilterState,
  page: number,
  pageSize: number = LEDGER_PAGE_SIZE,
): Record<string, unknown> | null {
  if (!filter.account_id) {
    return null;
  }
  const query: Record<string, unknown> = {
    account_id: filter.account_id,
    page,
    page_size: pageSize,
  };
  const from = filter.period_from.trim();
  const to = filter.period_to.trim();
  if (from) {
    query.period_from = from;
  }
  if (to) {
    query.period_to = to;
  }
  return query;
}

/** 期间区间校验：起止都填且起 > 止 → 返回提示（其余情况返回空串） */
export function ledgerRangeError(filter: Pick<LedgerFilterState, 'period_from' | 'period_to'>): string {
  const from = filter.period_from.trim();
  const to = filter.period_to.trim();
  if (from && to && from > to) {
    return '起始期间不能晚于结束期间';
  }
  return '';
}

/** 流水行累计（本页），返回每行「借 - 贷」的净额与合计 */
export interface LedgerPageTotals {
  /** 本页借方合计（分） */
  debitCents: number;
  /** 本页贷方合计（分） */
  creditCents: number;
  /** 本页净额（借 − 贷，分） */
  netCents: number;
  /** 行数 */
  count: number;
}

/** 本页合计（页脚展示；≠ 科目余额，余额看 running_balance / 余额表） */
export function ledgerPageTotals(rows: ReadonlyArray<Pick<LedgerRow, 'debit_amount' | 'credit_amount'>>): LedgerPageTotals {
  let debitCents = 0;
  let creditCents = 0;
  for (const row of rows) {
    debitCents += toCents(row.debit_amount);
    creditCents += toCents(row.credit_amount);
  }
  return { debitCents, creditCents, netCents: debitCents - creditCents, count: rows.length };
}

/**
 * 校验后端给的逐行余额是否自洽：`running[i] == running[i-1] + 借[i] - 贷[i]`。
 *
 * 第一行的期望值是 `startCents + 本行净额`，`startCents` 是「本页之前所有行的净额合计」
 * （第 2 页及以后由后端带出来；不知道就只能信任第一行，见 {@link verifyRunning} 的 `strict`）。
 *
 * @returns 不自洽的行下标（空数组 = 全对）
 */
export function runningBreaks(
  rows: ReadonlyArray<Pick<LedgerRow, 'debit_amount' | 'credit_amount' | 'running_balance'>>,
  startCents?: number,
): number[] {
  const breaks: number[] = [];
  if (rows.length === 0) {
    return breaks;
  }
  const netOf = (row: Pick<LedgerRow, 'debit_amount' | 'credit_amount'>): number =>
    toCents(row.debit_amount) - toCents(row.credit_amount);

  // 第一行：没有上一行可参照。
  // - 没给期初值 → 信任后端给的第一行余额（它已经含了本页之前所有行的净额），拿它当基准；
  // - 给了期初值 → 期初 + 本行净额必须等于第一行余额（翻页场景严格校验）。
  let expected = startCents === undefined ? toCents(rows[0]!.running_balance) : startCents + netOf(rows[0]!);
  if (startCents !== undefined && toCents(rows[0]!.running_balance) !== expected) {
    breaks.push(0);
  }
  for (let index = 1; index < rows.length; index += 1) {
    expected += netOf(rows[index]!);
    if (toCents(rows[index]!.running_balance) !== expected) {
      breaks.push(index);
    }
  }
  return breaks;
}

// ==================== Tab2：科目余额表 ====================

/** 单行的净额拆解（分） */
export interface TrialBalanceRowCheck {
  /** 科目编码（报错时指路） */
  account_code: string;
  /** 期初净额 = opening_debit − opening_credit */
  openingNetCents: number;
  /** 本期净额 = period_debit − period_credit */
  periodNetCents: number;
  /** 期末净额 = closing_debit − closing_credit */
  closingNetCents: number;
  /** 恒等式是否成立：期初净额 + 本期净额 == 期末净额 */
  ok: boolean;
}

/** 单行恒等式校验（暴露给单测与详情提示） */
export function checkTrialBalanceRow(row: TrialBalanceRow): TrialBalanceRowCheck {
  const openingNetCents = toCents(row.opening_debit) - toCents(row.opening_credit);
  const periodNetCents = toCents(row.period_debit) - toCents(row.period_credit);
  const closingNetCents = toCents(row.closing_debit) - toCents(row.closing_credit);
  return {
    account_code: row.account_code,
    openingNetCents,
    periodNetCents,
    closingNetCents,
    ok: openingNetCents + periodNetCents === closingNetCents,
  };
}

/** 余额表合计与平衡结论（**前端自己加一遍**，与后端 totals 对照） */
export interface TrialBalanceTotals {
  /** 行数 */
  count: number;
  /** 期初借方合计（分） */
  openingDebitCents: number;
  /** 期初贷方合计（分） */
  openingCreditCents: number;
  /** 本期借方合计（分） */
  periodDebitCents: number;
  /** 本期贷方合计（分） */
  periodCreditCents: number;
  /** 期末借方合计（分） */
  closingDebitCents: number;
  /** 期末贷方合计（分） */
  closingCreditCents: number;
  /** 本期是否平衡：Σ本期借 = Σ本期贷 */
  periodBalanced: boolean;
  /** 期末是否平衡：Σ期末借 = Σ期末贷 */
  closingBalanced: boolean;
  /** 期初是否平衡：Σ期初借 = Σ期初贷 */
  openingBalanced: boolean;
  /** 总判定：本期与期末都平（期初不平单独提示，那是历史问题） */
  balanced: boolean;
  /** 逐行恒等式不成立的行（空数组 = 每行都对） */
  brokenRows: string[];
}

/** 余额表合计 + 三重平衡判定 */
export function trialBalanceTotals(rows: ReadonlyArray<TrialBalanceRow>): TrialBalanceTotals {
  let openingDebitCents = 0;
  let openingCreditCents = 0;
  let periodDebitCents = 0;
  let periodCreditCents = 0;
  let closingDebitCents = 0;
  let closingCreditCents = 0;
  const brokenRows: string[] = [];
  for (const row of rows) {
    openingDebitCents += toCents(row.opening_debit);
    openingCreditCents += toCents(row.opening_credit);
    periodDebitCents += toCents(row.period_debit);
    periodCreditCents += toCents(row.period_credit);
    closingDebitCents += toCents(row.closing_debit);
    closingCreditCents += toCents(row.closing_credit);
    if (!checkTrialBalanceRow(row).ok) {
      brokenRows.push(row.account_code);
    }
  }
  const periodBalanced = periodDebitCents === periodCreditCents;
  const closingBalanced = closingDebitCents === closingCreditCents;
  const openingBalanced = openingDebitCents === openingCreditCents;
  return {
    count: rows.length,
    openingDebitCents,
    openingCreditCents,
    periodDebitCents,
    periodCreditCents,
    closingDebitCents,
    closingCreditCents,
    periodBalanced,
    closingBalanced,
    openingBalanced,
    balanced: periodBalanced && closingBalanced && brokenRows.length === 0,
    brokenRows,
  };
}

/** 合计行文案：平衡 → `✅ 平衡`；否则 → `⚠ 本期不平`（红） */
export function trialBalanceVerdict(totals: TrialBalanceTotals): string {
  if (totals.balanced) {
    return '平衡';
  }
  if (!totals.periodBalanced) {
    return `本期不平（差 ${(Math.abs(totals.periodDebitCents - totals.periodCreditCents) / 100).toFixed(2)}）`;
  }
  if (!totals.closingBalanced) {
    return `期末不平（差 ${(Math.abs(totals.closingDebitCents - totals.closingCreditCents) / 100).toFixed(2)}）`;
  }
  return `明细恒等式不成立：${totals.brokenRows.join('、')}`;
}

/** 期间必填校验：合法 `yyyy-MM` 返回空串，否则返回提示 */
export function trialBalancePeriodError(period: string): string {
  const text = period.trim();
  if (!text) {
    return '请选择会计期间（yyyy-MM）';
  }
  if (!isMonthKey(text)) {
    return '期间格式应为 yyyy-MM（例：2026-10）';
  }
  return '';
}

/** 默认期间（北京时间的当月，与后端 `today_cst` 同口径） */
export function defaultPeriod(): string {
  return todayPeriod();
}

/** 科目下拉标签：`1405 库存商品`（二级科目按层级缩进，用全角空格，不引入样式耦合） */
export function accountLabelOf(account: Pick<AccountRow, 'account_code' | 'account_name' | 'level'>): string {
  const indent = '　'.repeat(Math.max(0, (account.level ?? 1) - 1));
  return `${indent}${account.account_code} ${account.account_name}`;
}

/** 科目下拉候选（id 为值，标签带编码与缩进） */
export function accountOptionsOf(
  accounts: ReadonlyArray<Pick<AccountRow, 'id' | 'account_code' | 'account_name' | 'level'>>,
): Array<{ label: string; value: string }> {
  return accounts.map((account) => ({ label: accountLabelOf(account), value: account.id }));
}

/** 科目名（流水表头「当前科目」用；查不到退化成编码） */
export function accountTextOf(
  accountId: string,
  accounts: ReadonlyArray<Pick<AccountRow, 'id' | 'account_code' | 'account_name'>>,
): string {
  const hit = accounts.find((item) => item.id === accountId);
  return hit ? `${hit.account_code} ${hit.account_name}` : dash(accountId);
}
