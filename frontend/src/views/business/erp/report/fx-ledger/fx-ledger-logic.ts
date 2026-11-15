/**
 * 外币明细账的**纯逻辑**（M1-08 · 迁移 `V0413`）。
 *
 * 为什么单独一个 `.ts`：`index.vue` 只负责画，而「金额怎么格式化」「余额算借还是贷」
 * 「区间怎么显示」「空表怎么说」这些判据一旦写进模板，`vue-tsc` 抓不到漂移
 * （与 `fx-balance-logic.ts` / `fx-settings-logic.ts` 同一套做法）。这里全是无副作用纯函数，可单测。
 *
 * 口径与后端逐字对齐：
 * - 余额是**借方为正**的有符号值（`Decimal` 借 − 贷），负数 = 贷方余额，**不许取绝对值**；
 * - 余额是**按科目分别累计**的，页面必须同时显示科目列（跨科目不可连读）；
 * - 金额是十进制**字符串**（`NUMERIC(20,4)`），格式化**不许**经过 `Number()`；
 * - 「本位币」三列只含该币种原币行的本位币金额，**不含历次调汇**（含调汇看外币余额表）。
 *
 * 常量放在这里（不放 `api/…/fx-ledger.ts`）：那个文件 import 了 `@/api/request`，
 * 而 `@/api/request` 读 `import.meta.env`，`tsx --test` 的 node 环境里没有它 —— 单测
 * 只能 import **不碰运行时依赖**的模块（类型 import 会被擦除，常量放这里才测得动）。
 */
import type { FxLedgerReport, FxLedgerSide } from '@/api/business/erp/fx-ledger';

/** 页面权限码（与 `V0413` 的 `sys_menu.auth_code` 逐字一致；报表页码就是接口码） */
export const PERM_FX_LEDGER = 'business:erp:report:fx-ledger';

/** 报表编码（后端 `fx_ledger::REPORT_CODE`） */
export const FX_LEDGER_REPORT_CODE = 'FX_LEDGER';

/** 空值占位符（页面统一用它，别各写各的 `0` / `--` / `无`） */
export const EMPTY_PLACEHOLDER = '—';

/** 红字凭证标记（红字行净额为负，余额直接累加，不需要取绝对值） */
export const RED_LABEL = '红字';

/**
 * 金额格式化：千分位 + 2 位小数（**十进制字符串四舍五入**，不过 `Number()`）。
 *
 * 与 `report/fx-balance/fx-balance-logic.ts` 里的实现**逐字同算法**（那里又与
 * `report/balance-sheet` / `report/payable-aging` 同算法）：
 * `Number('0.595').toFixed(2)` 会得到 `0.59`，报表上少一分钱是事故。
 * 传 `null` / `''` 返回空串，由调用方决定显示占位符。
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

/** 金额单元格：`null` / 空串显示占位符（不显示 0） */
export function amountCell(value: string | null | undefined): string {
  const text = formatAmount(value);
  return text === '' ? EMPTY_PLACEHOLDER : text;
}

/** 余额方向（有符号余额 → 借 / 贷 / 平；阈值按符号，与后端 `side_of` 同口径） */
export function sideOf(balance: string | null | undefined): FxLedgerSide {
  if (balance === null || balance === undefined || balance === '') return 'ZERO';
  const text = String(balance).trim();
  if (/^-/.test(text)) {
    return /^0(\.0*)?$/.test(text.slice(1)) ? 'ZERO' : 'CREDIT';
  }
  return /^0(\.0*)?$/.test(text) ? 'ZERO' : 'DEBIT';
}

/** 借 / 贷 / 平（表头与行内方向标记用） */
export function sideLabel(side: FxLedgerSide): string {
  switch (side) {
    case 'DEBIT':
      return '借';
    case 'CREDIT':
      return '贷';
    default:
      return '平';
  }
}

/** 余额带方向的中文：「借 300.00」/「贷 300.00」/「平」 */
export function balanceText(balance: string | null | undefined): string {
  const side = sideOf(balance);
  if (side === 'ZERO') return '平';
  const text = formatAmount(balance);
  return `${sideLabel(side)} ${text.replace(/^-/, '')}`;
}

/** 币种显示名：「USD 美元」/ 主数据缺失时只有「USD」 */
export function currencyLabel(code: string, name: string | null): string {
  return name ? `${code} ${name}` : code;
}

/** 汇率显示：8 位小数（后端 `NUMERIC(18,8)`） */
export function rateText(rate: string | null | undefined): string {
  if (rate === null || rate === undefined || rate === '') return EMPTY_PLACEHOLDER;
  const text = String(rate).trim();
  const [intPart, frac = ''] = text.split('.');
  return `${intPart}.${`${frac}00000000`.slice(0, 8)}`;
}

/** 日期区间文字：`2026-11-01 ~ 2026-11-30` / `2026-11-01 起` / `截至 2026-11-30` / `全部历史` */
export function rangeText(
  report: Pick<FxLedgerReport, 'date_from' | 'date_to'> & Partial<Pick<FxLedgerReport, 'hints'>>,
): string {
  const from = report.date_from;
  const to = report.date_to;
  if (from && to) return `${from} ~ ${to}`;
  if (from) return `${from} 起`;
  if (to) return `截至 ${to}`;
  return '全部历史';
}

/**
 * 本位币列的说明（页面表头 tooltip 用）。
 *
 * 这条是硬口径：本表只列该币种的原币行，期末调汇是本位币凭证、**不在**本表，
 * 所以这里的「本位币发生额」不等于「含调汇的账面」。不写清楚会有人拿它对账面。
 */
export function bookColumnHint(currency: string, baseCurrency: string): string {
  return (
    `只累计 ${currency} 原币行的本位币金额；期末调汇（本位币凭证）不在本表 —— ` +
    `含历次调汇的账面余额请看「外币余额表」（币种 ${baseCurrency} 口径）`
  );
}

/** 逐行余额的说明（按科目累计，跨科目不可连读） */
export function runningColumnHint(): string {
  return '逐行余额按科目分别累计，含该科目区间首日之前的期初；不同科目的行不可直接连读';
}

/** 空表时的一句话（区分「该币种本期没业务」与「筛了未标记外币核算的科目」） */
export function emptyText(
  report: Pick<FxLedgerReport, 'currency' | 'account' | 'date_from' | 'date_to'>,
): string {
  const range = rangeText(report);
  if (report.account && !report.account.is_foreign_currency) {
    return `科目 ${report.account.account_code} ${report.account.account_name} 未标记「外币核算」，` +
      '不产生外币明细账（请先到科目页标记）';
  }
  if (report.account) {
    return `科目 ${report.account.account_code} 在 ${range} 没有 ${report.currency} 的已过账分录（这不是出错）`;
  }
  return `币种 ${report.currency} 在 ${range} 没有已过账的外币分录（这不是出错）`;
}

/** 合计行的方向文字（期末净头寸），例如「借 300.00」 */
export function closingTotalText(
  report: Pick<FxLedgerReport, 'closing_fc' | 'closing_side'>,
): string {
  if (report.closing_side === 'ZERO') return '平';
  return `${sideLabel(report.closing_side)} ${formatAmount(report.closing_fc).replace(/^-/, '')}`;
}
