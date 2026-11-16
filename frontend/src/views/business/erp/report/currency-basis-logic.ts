/**
 * 报表币种切换的**纯逻辑**（M1-09）：资产负债表 / 利润表共用。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么单独一个文件
 *
 * 两张报表页都要回答同样五个问题：**是不是外币口径 / 折合列的表头写什么 /
 * 页头「币种口径」那句话怎么说 / 导出文件名带什么后缀 / 下拉里有哪些选项**。
 * 写在 `.vue` 里就得抄两遍 —— 抄第二遍时口径就开始漂移（一处写「原币」，一处写「币种」，
 * 用户以为两张报表口径不同）。所以口径判断全部收在这里，页面只负责摆位置。
 *
 * # 口径来源
 *
 * 全部字段来自后端 `report::fx_report::ReportCurrencyRes`（`fetchBalanceSheet` /
 * `fetchIncomeStatement` 的 `currency` 块）。**前端不猜**：`mode` 是后端给的，
 * 不靠「币种 != 本位币」自己推；缺牌价看 `rate_missing`，不靠 `rate == null` 自己推。
 */

/** 本位币口径（与 M1-09 之前逐字一致）。 */
export const CURRENCY_MODE_BASE = 'BASE';

/** 外币原币口径。 */
export const CURRENCY_MODE_FOREIGN = 'FOREIGN';

/** 报表币种块（后端 `ReportCurrencyRes` 的前端视图；字段与后端逐字对齐）。 */
export interface ReportCurrencyLike {
  mode: string;
  currency: string;
  currency_name?: string | null;
  base_currency: string;
  rate?: string | null;
  rate_date?: string | null;
  rate_missing?: boolean;
  entry_count?: number;
}

/** 下拉选项（`value = ''` 表示本位币口径，不传 `currency` 参数）。 */
export interface CurrencySelectOption {
  label: string;
  value: string;
}

/** 后端币种选项（`/erp/currency/options` 的子集，只取这里要用的字段）。 */
export interface CurrencyOptionLike {
  code: string;
  label: string;
  is_base?: boolean;
}

/** 是不是外币原币口径（决定要不要出「折合本位币」列）。 */
export function isForeignMode(currency?: ReportCurrencyLike | null): boolean {
  return currency?.mode === CURRENCY_MODE_FOREIGN;
}

/** 折合列 / 折合表头里该写的本位币码（拿不到时退回「本位币」，绝不写死 CNY）。 */
export function baseCurrencyLabel(currency?: ReportCurrencyLike | null): string {
  return currency?.base_currency || '本位币';
}

/**
 * 页头「币种口径」文案。
 *
 * 三种状态各说一句人话：
 * - 本位币 ⇒ `本位币 CNY`；
 * - 外币 + 有牌价 ⇒ `原币 USD（美元）· 折算 7.1`；
 * - 外币 + 缺牌价 ⇒ `原币 USD（美元）· 缺期末牌价，折合列留空`（**必须**写「留空」，
 *   否则用户看到空白折合列会以为报表坏了）。
 */
export function currencyModeText(currency?: ReportCurrencyLike | null): string {
  if (!currency) return '';
  const name = currency.currency_name ? `（${currency.currency_name}）` : '';
  if (!isForeignMode(currency)) {
    return `本位币 ${currency.base_currency || currency.currency}`;
  }
  if (currency.rate_missing || !currency.rate) {
    return `原币 ${currency.currency}${name}·缺期末牌价，折合列留空`;
  }
  return `原币 ${currency.currency}${name}·折算 ${currency.rate}`;
}

/** 折合列表头：`折合CNY` / （本位币口径时用不到，调用方 `v-if` 掉）。 */
export function convertedHeader(currency?: ReportCurrencyLike | null): string {
  return `折合${baseCurrencyLabel(currency)}`;
}

/**
 * 导出文件名后缀（外币口径带币种，本位币口径为空）。
 *
 * 为什么必须带：同一天导两张报表（本位币 + 美元），文件名一样会互相覆盖，
 * 而两张表的数字**完全不同** —— 覆盖了就是把报表弄丢。
 */
export function exportCurrencySuffix(currencyCode?: string | null): string {
  const code = (currencyCode ?? '').trim().toUpperCase();
  return code ? `_${code}` : '';
}

/** 币种下拉选项：本位币 + 全部启用的外币（`is_base` 那条不重复列）。 */
export function buildCurrencyOptions(list: CurrencyOptionLike[]): CurrencySelectOption[] {
  return [
    { label: '本位币', value: '' },
    ...list
      .filter((item) => !item.is_base)
      .map((item) => ({ label: `${item.label}（原币）`, value: item.code })),
  ];
}
