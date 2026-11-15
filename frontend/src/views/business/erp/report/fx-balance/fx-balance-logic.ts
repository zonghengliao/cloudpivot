/**
 * 外币余额表的**纯逻辑**（M1-07 · 迁移 `V0412`）。
 *
 * 为什么单独一个 `.ts`：`index.vue` 只负责画，而「金额怎么格式化」「余额算借还是贷」
 * 「缺牌价显示什么」「合计能不能显示」这些判据一旦写进模板，`vue-tsc` 抓不到漂移
 * （与 `exchange-rate/fx-settings-logic.ts` 同一套做法）。这里全是无副作用纯函数，可单测。
 *
 * 口径与后端逐字对齐：
 * - 余额是**借方为正**的有符号值（`Decimal` 借 − 贷），负数 = 贷方余额，**不许取绝对值**；
 * - 金额是十进制**字符串**（`NUMERIC(20,4)`），格式化**不许**经过 `Number()`；
 * - `converted_base` / `reval_diff` 为 `null` = 缺期末即期汇率，后端**没有**按 rate = 1 折算；
 * - `total_converted_base` 为 `null` = 至少一个币种缺牌价（半套折算的合计不能显示）。
 *
 * 常量放在这里（不放 `api/…/fx-balance.ts`）：那个文件 import 了 `@/api/request`，
 * 而 `@/api/request` 读 `import.meta.env`，`tsx --test` 的 node 环境里没有它 —— 单测
 * 只能 import **不碰运行时依赖**的模块（类型 import 会被擦除，常量放这里才测得动）。
 */
import type {
  FxBalanceReport,
  FxBalanceSide,
  FxBookBasis,
  FxPeriodStatus,
} from '@/api/business/erp/fx-balance';

/** 页面权限码（与 `V0412` 的 `sys_menu.auth_code` 逐字一致；报表页码就是接口码） */
export const PERM_FX_BALANCE = 'business:erp:report:fx-balance';

/** 报表编码（后端 `fx_balance::REPORT_CODE`） */
export const FX_BALANCE_REPORT_CODE = 'FX_BALANCE';

/** 缺牌价占位符（页面统一用它，别各写各的 `0` / `--` / `无`） */
export const MISSING_RATE_PLACEHOLDER = '—';

/**
 * 金额格式化：千分位 + 2 位小数（**十进制字符串四舍五入**，不过 `Number()`）。
 *
 * 与 `report/balance-sheet` / `report/payable-aging` 页面里的实现逐字同算法
 * （`Number('0.595').toFixed(2)` 会得到 `0.59`，报表上少一分钱是事故）。
 * 传 `null` / `''`（缺牌价的折算列）返回空串，由调用方决定显示占位符。
 */
export function formatAmount(value: string | null | undefined): string {
  if (value === null || value === undefined || value === '') return '';
  const text = String(value).trim();
  const negative = text.startsWith('-');
  const clean = negative ? text.slice(1) : text;
  const [rawInt, rawFrac = ''] = clean.split('.');
  const intPart = rawInt || '0';
  const frac4 = `${rawFrac}0000`.slice(0, 4);

  let digits = (intPart + frac4.slice(0, 2)).split('').map(Number);
  if (frac4[2] >= '5') {
    let index = digits.length - 1;
    while (index >= 0) {
      digits[index] += 1;
      if (digits[index] === 10) {
        digits[index] = 0;
        index -= 1;
      } else {
        break;
      }
    }
    if (index < 0) digits = [1, ...digits];
  }
  const joined = digits.join('') || '0';
  const cents = joined.slice(-2).padStart(2, '0');
  const yuan = (joined.length > 2 ? joined.slice(0, -2) : '0').replace(/^0+(?=\d)/, '');
  const grouped = yuan.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  const isZero = /^0+$/.test(joined);
  return `${negative && !isZero ? '-' : ''}${grouped}.${cents}`;
}

/**
 * 报表金额单元格：`null` 一律显示占位符（缺牌价），**不显示 0**。
 *
 * 这条是硬口径：把 `null` 画成 `0.00` 会让财务以为「这个币种折算是 0 元」。
 */
export function amountCell(value: string | null | undefined): string {
  const text = formatAmount(value);
  return text === '' ? MISSING_RATE_PLACEHOLDER : text;
}

/** 余额方向（有符号余额 → 借 / 贷 / 平），阈值按 4 位小数（与后端 `side_of` 同口径） */
export function sideOf(balance: string | null | undefined): FxBalanceSide {
  if (balance === null || balance === undefined || balance === '') return 'ZERO';
  const text = String(balance).trim();
  // 只判符号与「是否全 0」，不经过浮点数：`-.0001` 也是贷方
  if (/^-/.test(text)) {
    return /^0(\.0*)?$/.test(text.slice(1)) ? 'ZERO' : 'CREDIT';
  }
  return /^0(\.0*)?$/.test(text) ? 'ZERO' : 'DEBIT';
}

/** 借 / 贷 / 平（表头与行内方向标记用） */
export function sideLabel(side: FxBalanceSide): string {
  switch (side) {
    case 'DEBIT':
      return '借';
    case 'CREDIT':
      return '贷';
    default:
      return '平';
  }
}

/** 余额带方向的中文：「借 2,120.00」/「贷 2,070.00」/「平」 */
export function balanceText(balance: string | null | undefined): string {
  const side = sideOf(balance);
  if (side === 'ZERO') return '平';
  const text = formatAmount(balance);
  return `${sideLabel(side)} ${text.replace(/^-/, '')}`;
}

/** 账面口径的说明（多币种科目退化时页面必须写清「这不是整科目的账面」） */
export function basisLabel(basis: FxBookBasis): string {
  return basis === 'CURRENCY' ? '该币种账面' : '科目合计';
}

/** 账面口径的 tooltip 文案 */
export function basisHint(basis: FxBookBasis): string {
  return basis === 'CURRENCY'
    ? '该科目同时挂了多个外币币种：本位币分录没有币种归属、无法按币种分摊，这里退到「该币种分录的账面」'
    : '该科目只有一个外币币种：账面取科目全部已过账分录（含本位币分录与历次调汇），与期末调汇同口径';
}

/** 币种显示名：「USD 美元」/ 主数据缺失时只有「USD」 */
export function currencyLabel(code: string, name: string | null): string {
  return name ? `${code} ${name}` : code;
}

/** 汇率显示：8 位小数（后端 `NUMERIC(18,8)`）；缺牌价显示占位符 */
export function rateText(rate: string | null): string {
  if (rate === null || rate === '') return MISSING_RATE_PLACEHOLDER;
  const text = String(rate).trim();
  const [intPart, frac = ''] = text.split('.');
  return `${intPart}.${`${frac}00000000`.slice(0, 8)}`;
}

/** 期间状态中文（`null` = 该月未建账，后端按自然月推算） */
export function periodStatusText(status: FxPeriodStatus | null): string {
  switch (status) {
    case 'OPEN':
      return '未结账';
    case 'CLOSED':
      return '已结账';
    default:
      return '未建账·按自然月推算';
  }
}

/** 汇兑差额的方向说明（正 = 借方调增，负 = 贷方调减） */
export function diffText(diff: string | null): string {
  if (diff === null) return MISSING_RATE_PLACEHOLDER;
  const side = sideOf(diff);
  if (side === 'ZERO') return '0.00';
  const amount = formatAmount(diff).replace(/^-/, '');
  return side === 'DEBIT' ? `调增 ${amount}` : `调减 ${amount}`;
}

/** 合计区的折算合计能不能显示（缺任一币种牌价 ⇒ 不显示，见文件头） */
export function convertedTotalText(report: Pick<FxBalanceReport, 'total_converted_base' | 'missing_rates'>): string {
  if (report.total_converted_base === null) {
    return MISSING_RATE_PLACEHOLDER;
  }
  return formatAmount(report.total_converted_base);
}

/** 合计区的汇兑差额合计 */
export function diffTotalText(report: Pick<FxBalanceReport, 'total_reval_diff'>): string {
  return report.total_reval_diff === null ? MISSING_RATE_PLACEHOLDER : formatAmount(report.total_reval_diff);
}

/** 缺牌价时给用户的一句话（点名币种 + 去哪补），没有缺牌价返回 `null` */
export function missingRateText(report: Pick<FxBalanceReport, 'missing_rates' | 'period_end'>): string | null {
  if (!report.missing_rates.length) return null;
  return (
    `币种 ${report.missing_rates.join('、')} 在 ${report.period_end} 之前没有即期汇率：` +
    '折算与差额列留空（后端没有按汇率 1 折算），请先到「汇率管理」补牌价'
  );
}

/** 空表时的一句话（区分「本来就没有外币业务」与「筛了本位币」） */
export function emptyText(report: Pick<FxBalanceReport, 'currency_filter'>, baseCurrency: string): string {
  const filter = report.currency_filter;
  if (filter && filter === baseCurrency) {
    return `${filter} 是账套本位币，不构成外币余额（本表只列外币核算科目）`;
  }
  if (filter) {
    return `${filter} 本期没有外币核算科目的余额与发生额（这不是出错）`;
  }
  return '本期没有外币核算科目的余额与发生额（这不是出错）';
}
