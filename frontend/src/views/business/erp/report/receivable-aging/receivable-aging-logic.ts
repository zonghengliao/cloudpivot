/**
 * 应收账龄分析表（M1-23）的**纯逻辑**。
 *
 * 为什么单独一个 `.ts`：`index.vue` 只负责画。把「金额怎么格式化」「客户怎么排」
 * 「明细看谁」「403 怎么说人话」这些判据写进模板，`vue-tsc` 抓不到漂移
 * （与 `report/fx-balance/fx-balance-logic.ts` 同一套做法）。这里全是无副作用纯函数，可单测。
 *
 * # 口径（逐字照后端 `receivable_aging.rs`，不猜）
 *
 * - 金额是十进制**字符串**（`NUMERIC` → `rust_decimal` → serde 字符串）。格式化**不许**过
 *   `Number()`：`Number('0.595').toFixed(2)` 得 `0.59`，报表少一分钱是事故。排序也不许过
 *   `Number()` —— 20 位小数的单据金额（大额合同分期）会丢精度，见 {@link compareAmountDesc}。
 * - 分档是 **30/60/90/180/365 七档**（后端 `payable_aging::AGING_BUCKETS`）。表头一律按响应
 *   `buckets[]` 渲染，前端**不写死**档位与阈值；这里只提供配色，认不出的键回落灰色。
 * - 一格金额字段名是 **`unpaid`**（后端与应付共用 `AgingBucketAmountRes`），不是 `unsettled`。
 * - `total_unsettled` = 全表未收合计（后端 SQL 聚合，不受明细 1000 条截断影响）。
 *   页面按关键字筛客户时**不重算合计**：客户端加总金额 = 第二套账（见 {@link filterCustomers}）。
 * - 逾期天数 `overdue_days` = 基准日 − 到期日：正数 = 逾期，0 = 到期当天，负数 = 未到期。
 *
 * # 两个权限码（别合并）
 *
 * | 码 | 管什么 |
 * |----|--------|
 * | `business:erp:report:receivable-aging` | **页面**（菜单显形 + 路由 `meta.permission`） |
 * | `business:erp:receivable:aging`        | **取数接口** `GET /receivable/aging` |
 *
 * 报表页与接口各有一个码是事实（页面行 V0418 / 按钮行 V0288）；
 * {@link PERM_RECEIVABLE_AGING_DATA} 只用来在 403 时把「缺哪个码」说清楚，
 * 页面**不拿它做按钮显隐**（前端硬判权限只会与后端漂移）。
 */
import type {
  AccountPeriodRow,
  ReceivableAging,
  ReceivableAgingCustomer,
  ReceivableAgingDetailRow,
} from '@/api/business/erp';

/** 页面显形码（迁移 `V0418` 的 `sys_menu.auth_code` 逐字一致；也在 `router/index.ts` 的 `meta.permission`） */
export const PERM_RECEIVABLE_AGING_PAGE = 'business:erp:report:receivable-aging';

/** 取数接口码（后端 `receivable_aging.rs` 的 `#[nik::permission]`；V0288 种的按钮行） */
export const PERM_RECEIVABLE_AGING_DATA = 'business:erp:receivable:aging';

/** 报表编码（后端 `receivable_aging::REPORT_CODE`） */
export const RECEIVABLE_AGING_REPORT_CODE = 'RECEIVABLE_AGING';

/** 明细接口的截断上限（后端 `payable_aging::DETAIL_LIMIT`），用于「本页仅列前 N 张」的说法 */
export const DETAIL_LIMIT = 1000;

// ==================== 金额 ====================

/**
 * 千分位 + 2 位小数（**十进制字符串四舍五入**，不过 `Number()`）。
 *
 * 与 `report/payable-aging` / `report/balance-sheet` / `report/fx-balance` 里的实现逐字同算法：
 * 两端必须给出同样的数字，否则同一张表在两个页面显示不同金额。
 * `null` / `''` 返回空串（本页的金额列不会是 null —— 后端缺值是 `'0.0000'`，不是 null）。
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

/** 金额是否为零（`'0.0000'` / `'0'` / `''` 都算 0）—— 零格显示成灰色，别让 0.00 抢眼 */
export function isZeroAmount(value: string | null | undefined): boolean {
  const text = String(value ?? '').trim();
  if (!text) return true;
  return /^-?0*(\.0*)?$/.test(text);
}

/**
 * 十进制字符串的**精确**比较（返回 −1 / 0 / 1）。**不过 `Number()`**。
 *
 * 为什么不用 `Number(a) - Number(b)`：单据金额是 `NUMERIC(20,4)`，
 * `Number` 只有 15-17 位有效数字，`1000000000000000.0001` 与 `1000000000000000.0002`
 * 会判成相等 —— 客户排序就会在毫不知情的情况下乱。
 */
export function compareDecimalStrings(a: string | null | undefined, b: string | null | undefined): number {
  const left = normalizeDecimal(a);
  const right = normalizeDecimal(b);
  if (left.negative !== right.negative) return left.negative ? -1 : 1;
  const sign = left.negative ? -1 : 1;
  // 先比整数位（去前导零后长度 → 字典序），再比小数位（右补零到同长后字典序）
  if (left.int.length !== right.int.length) return left.int.length > right.int.length ? sign : -sign;
  if (left.int !== right.int) return left.int > right.int ? sign : -sign;
  const [leftFrac, rightFrac] = padFrac(left.frac, right.frac);
  if (leftFrac !== rightFrac) return leftFrac > rightFrac ? sign : -sign;
  return 0;
}

/** 小数归一：符号 / 去前导零的整数位 / 去尾零的小数位（比较专用，不做舍入） */
function normalizeDecimal(value: string | null | undefined): {
  negative: boolean;
  int: string;
  frac: string;
} {
  const text = String(value ?? '').trim() || '0';
  const negative = text.startsWith('-');
  const clean = negative || text.startsWith('+') ? text.slice(1) : text;
  const [rawInt = '', rawFrac = ''] = clean.split('.');
  const int = rawInt.replace(/^0+/, '') || '0';
  const frac = rawFrac.replace(/0+$/, '');
  return { negative: negative && !(int === '0' && frac === ''), int, frac };
}

/** 小数位右补零到同长（比较时用；两边同长才能直接比字典序） */
function padFrac(a: string, b: string): [string, string] {
  const width = Math.max(a.length, b.length);
  return [a.padEnd(width, '0'), b.padEnd(width, '0')];
}

// ==================== 档位 / 逾期 ====================

/**
 * 一格金额占该行 / 合计的比例（0-100，一位小数），**只用于画像素宽度**。
 *
 * 这里的 `Number()` 只影响进度条长短，不参与任何会计断言；真金额一律 {@link formatAmount}。
 */
export function bucketShare(
  amount: string | number | null | undefined,
  total: string | number | null | undefined,
): number {
  const part = Number(amount ?? 0) || 0;
  const whole = Number(total ?? 0) || 0;
  if (whole <= 0 || part <= 0) return 0;
  return Math.min(100, Math.round((part / whole) * 1000) / 10);
}

/** 逾期天数文案：正数逾期 / 0 到期当天 / 负数未到期（与应付账龄页同一套说法） */
export function overdueText(days: number): string {
  if (days > 0) return `逾期 ${days}`;
  if (days === 0) return '到期当天';
  return `未到期 ${-days}`;
}

/** 档位配色键（TDesign tag theme）：认不出的键回落 `default`，后端加档位时最多灰色，不白屏 */
export function bucketThemeOf(key: string): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (key) {
    case 'NOT_DUE':
      return 'success';
    case 'D0_30':
      return 'primary';
    case 'D31_60':
    case 'D61_90':
      return 'warning';
    case 'D91_180':
    case 'D181_365':
    case 'OVER_365':
      return 'danger';
    // 旧 5 档口径的兜底键（历史响应 / 缓存），保留映射不删
    case 'OVER_90':
      return 'danger';
    default:
      return 'default';
  }
}

/** 全表最长逾期天数（合计行「最长逾期」列 = 各客户最大值，不是各自相加） */
export function maxOverdueDays(report: Pick<ReceivableAging, 'customers'> | null | undefined): number {
  if (!report) return 0;
  return report.customers.reduce((max, item) => Math.max(max, item.max_overdue_days), 0);
}

// ==================== 文字 / 枚举 ====================

/** 账龄基准日来源中文（后端 `as_of_source`） */
export function asOfSourceText(source: string): string {
  if (source === 'QUERY') return '手动指定';
  if (source === 'PERIOD') return '期间末日';
  if (source === 'TODAY') return '今天';
  return source || '—';
}

/** 期间状态中文（后端 `ReportPeriodStatus`；`DERIVED` = 未开账按自然月推算） */
export function periodStatusText(status: string | null | undefined): string {
  if (status === 'CLOSED') return '已结账';
  if (status === 'DERIVED') return '未开账·按自然月推算';
  if (status === 'OPEN' || status === 'NOT_CLOSED') return '未结账';
  return status || '';
}

/** 应收单状态中文（`receivable::STATUSES`：OPEN 未收 / PARTIAL 部分已收 / SETTLED 已收清 /
 *  WRITTEN_OFF 坏账核销 / CANCELLED 已作废）。账龄里只会出现前两种。 */
export function statusText(status: string): string {
  switch (status) {
    case 'OPEN':
      return '未收';
    case 'PARTIAL':
      return '部分已收';
    case 'SETTLED':
      return '已收清';
    case 'WRITTEN_OFF':
      return '坏账核销';
    case 'CANCELLED':
      return '已作废';
    default:
      return status || '—';
  }
}

/** 来源类型中文（`SALES_OUT` 出库过账 / `MANUAL` 手工挂账） */
export function sourceTypeText(sourceType: string): string {
  if (sourceType === 'SALES_OUT') return '销售出库';
  if (sourceType === 'MANUAL') return '手工挂账';
  return sourceType || '—';
}

/** `yyyy-MM-ddTHH:mm:ss` → `yyyy-MM-dd HH:mm:ss`（解析失败原样返回，不吞后端给的值） */
export function formatDateTime(value: string): string {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

// ==================== 客户行：筛 / 排 ====================

/** 客户汇总排序方式（三种都不涉及金额加总，只改顺序） */
export type CustomerSortMode = 'SERVER' | 'AMOUNT_DESC' | 'OVERDUE_DESC' | 'NAME_ASC';

/** 排序下拉的选项（`SERVER` = 后端原序，后端按未收余额倒序给） */
export const CUSTOMER_SORT_OPTIONS: { label: string; value: CustomerSortMode }[] = [
  { label: '未收降序（后端原序）', value: 'SERVER' },
  { label: '最长逾期降序', value: 'OVERDUE_DESC' },
  { label: '客户名升序', value: 'NAME_ASC' },
];

/**
 * 客户名关键字匹配（大小写不敏感、两端空白先去掉；空关键字 = 全命中）。
 *
 * 只匹配 `customer_name`：搜索框上写的就是「客户」，顺手也搜单号会让人以为搜到了明细。
 */
export function matchCustomer(customer: Pick<ReceivableAgingCustomer, 'customer_name'>, keyword: string): boolean {
  const needle = (keyword ?? '').trim().toLowerCase();
  if (!needle) return true;
  const name = (customer.customer_name ?? '').trim().toLowerCase();
  return name.includes(needle);
}

/**
 * 按关键字筛客户行。**只改「显示哪些客户行」，不重算任何合计**。
 *
 * 合计行永远显示后端给的 `total_unsettled`（全表 SQL 聚合）。按筛选结果客户端加总 = 第二套账：
 * 明细有 1000 条截断、档位口径在库里，前端加出来的数一旦与后端口径差一点，
 * 报表上就会出现「页头一个数、合计行另一个数」—— 那比不筛更糟。
 */
export function filterCustomers(
  customers: ReceivableAgingCustomer[],
  keyword: string,
): ReceivableAgingCustomer[] {
  return customers.filter((customer) => matchCustomer(customer, keyword));
}

/**
 * 客户行排序（返回新数组，不原地改）。
 *
 * - `SERVER`：后端原序（后端已按未收余额倒序），这里**原样复制**；
 * - `AMOUNT_DESC`：按未收合计倒序，金额用 {@link compareDecimalStrings} 精确比较；
 * - `OVERDUE_DESC`：最长逾期天数倒序；
 * - `NAME_ASC`：客户名升序（空名排最后，不抛错）。
 *
 * 同值保持原相对顺序（稳定排序）—— 侧栏/表格顺序每次刷新都变是最烦的体验。
 */
export function sortCustomers(
  customers: ReceivableAgingCustomer[],
  mode: CustomerSortMode,
): ReceivableAgingCustomer[] {
  const copy = [...customers];
  if (mode === 'AMOUNT_DESC') {
    copy.sort((a, b) => compareDecimalStrings(b.total_unsettled, a.total_unsettled));
  } else if (mode === 'OVERDUE_DESC') {
    copy.sort((a, b) => b.max_overdue_days - a.max_overdue_days);
  } else if (mode === 'NAME_ASC') {
    copy.sort((a, b) => {
      const left = (a.customer_name ?? '').trim();
      const right = (b.customer_name ?? '').trim();
      if (!left && !right) return 0;
      if (!left) return 1;
      if (!right) return -1;
      return left.localeCompare(right, 'zh-Hans-CN');
    });
  }
  return copy;
}

/** 页面实际渲染的客户行 = 先筛后排（一次调用，避免模板里串两个函数看不清顺序） */
export function arrangeCustomers(
  customers: ReceivableAgingCustomer[],
  keyword: string,
  mode: CustomerSortMode,
): ReceivableAgingCustomer[] {
  return sortCustomers(filterCustomers(customers, keyword), mode);
}

/** 明细按客户收窄（`null` / 空串 = 全部客户） */
export function detailsForCustomer(
  details: ReceivableAgingDetailRow[],
  customerId: string | null | undefined,
): ReceivableAgingDetailRow[] {
  if (!customerId) return details;
  return details.filter((row) => row.customer_id === customerId);
}

/** 明细区标题里的「谁 / 多少张」——避免模板里拼三段 v-if */
export function detailsScopeText(
  visibleCount: number,
  totalCount: number,
  customerName: string | null | undefined,
  truncated: boolean,
): string {
  const who = customerName ? `客户「${customerName}」` : '全部客户';
  const head = `${who} ${visibleCount} 张`;
  const total = visibleCount === totalCount ? `共 ${totalCount} 张` : `已筛出 ${visibleCount} / 共 ${totalCount} 张`;
  const tail = truncated ? '（明细接口最多回 1000 张，更早的单据未列出）' : '';
  return `${head}（${total}）${tail}`;
}

// ==================== 期间下拉 ====================

/**
 * 会计期间下拉：只列**已建立**的期间（`NOT_OPENED` 的查出来必然是空表，列出来只会误导），
 * 按 `period_code` 倒序（最近期间在最上面）。标签带结账状态，省一次点击。
 */
export function periodsToOptions(rows: AccountPeriodRow[]): { label: string; value: string }[] {
  return rows
    .filter((row) => row.status !== 'NOT_OPENED')
    .slice()
    .sort((a, b) => b.period_code.localeCompare(a.period_code))
    .map((row) => ({
      label: `${row.period_code}（${periodStatusText(row.status)}）`,
      value: row.period_code,
    }));
}

// ==================== 导出 / 报错 ====================

/**
 * 导出的兜底文件名（后端 `Content-Disposition` 里已有名字，这里只在拿不到时用）。
 * 与 `receivable-logic::agingExportFileName` 同一套命名：
 * `应收账款账龄分析表_<期间>_截止<基准日>.xlsx`。
 */
export function agingExportFileName(report: Pick<ReceivableAging, 'as_of' | 'period'>): string {
  return `应收账款账龄分析表_${report.period || '无期间'}_截止${report.as_of}.xlsx`;
}

/**
 * 把请求异常翻成**人话**（页头红字）。
 *
 * 403 单独说清楚：本页要两个码（页面码 + 取数码），最常见的事故就是
 * 「菜单授了、取数码没授」—— 页面能进、表出不来，而报错只写「无权限」四个字，
 * 用户会去找运维。这里直接点名缺的是哪个码。
 */
export function requestErrorMessage(err: unknown): string {
  const raw = err instanceof Error ? err.message : String(err ?? '');
  const text = (raw || '').trim();
  if (!text) return '查询失败（后端没有返回原因）';
  if (/\b403\b|无权限|权限不足|forbidden/i.test(text)) {
    return (
      `没有取数权限：本页需要 ${PERM_RECEIVABLE_AGING_DATA}。` +
      '页面能打开但表格是空的，通常是「菜单授了、取数码没授」—— 请管理员在角色里补上这个权限码。'
    );
  }
  return text;
}
