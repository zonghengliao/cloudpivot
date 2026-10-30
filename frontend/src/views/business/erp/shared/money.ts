/**
 * ERP 页面共用的金额 / 日期小工具（P2-E2）。
 *
 * # 为什么不直接复用 `purchase/purchase-logic.ts::formatMoney`
 *
 * 那是采购页的页面级逻辑文件，名字里带 `purchase`，被应付 / 凭证 / 总账三页
 * 反向依赖会让「改采购口径要看三个页面」；金额归一又是**会算错钱**的地方，
 * 需要一个单一、被单测钉住的实现。这里只放纯数值函数，不放任何业务口径。
 *
 * # 金额一律走「整数分」
 *
 * 后端 `rust_decimal::Decimal` 出 JSON 是字符串（`"226.0000"`），前端是 IEEE754 double。
 * `0.1 + 0.2 !== 0.3`，所以**比较与累加都先换成整数分**（`toCents`），
 * 只在显示前换回来 —— 借贷平衡判断要是用浮点比，`226.00000000000003` 这种
 * 会假报警，而财报上的「平」必须是精确的。
 */

/**
 * 后端 NUMERIC 出 JSON 可能是 `number` 也可能是字符串（`"226.0000"`）。
 *
 * ⚠️ 直接用 `+` 会得到字符串拼接（`"226.0000" + 1`），统一过这里。
 */
export function toNumber(value: number | string | null | undefined): number {
  if (value === null || value === undefined || value === '') {
    return 0;
  }
  const parsed = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/**
 * 金额 → 整数分（四舍五入到 2 位）。
 *
 * `Math.round(-0.5)` 得 `-0`（向 +∞ 取整），与后端 `Decimal::round_dp` 的
 * half-away-from-zero 在负数中点处不一致，所以负数先取绝对值再补号。
 */
export function toCents(value: number | string | null | undefined): number {
  const num = toNumber(value);
  if (!Number.isFinite(num)) {
    return 0;
  }
  const scaled = Math.abs(num) * 100;
  const rounded = Math.round(scaled + 1e-7);
  if (rounded === 0) {
    // 避免 `-0`：`-0.001` 舍入后是 0，但取号会得到 -0，
    // 而 `Object.is(-0, 0) === false` —— 平衡判断里一个 -0 就能让「平」变「不平」。
    return 0;
  }
  return num < 0 ? -rounded : rounded;
}

/** 整数分 → 元（两位小数的 number；仅用于展示/断言，不要拿去累加） */
export function centsToNumber(cents: number): number {
  return cents / 100;
}

/**
 * 金额格式化：`226.00` / `12,345.60`（三位一组，固定 2 位，负数带 `-`）。
 *
 * 入参是整数分（见 {@link toCents}），避免「先转 number 再 toFixed」的二次舍入。
 */
export function formatCents(cents: number): string {
  const negative = cents < 0;
  const fixed = (Math.abs(Math.round(cents)) / 100).toFixed(2);
  const [intPart, decPart] = fixed.split('.');
  const grouped = (intPart ?? '0').replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${negative ? '-' : ''}${grouped}.${decPart ?? '00'}`;
}

/** `formatCents(toCents(x))` 的快捷方式（列表单元格直接用） */
export function formatMoney(value: number | string | null | undefined): string {
  return formatCents(toCents(value));
}

/** 日期 `YYYY-MM-DD` → `MM-DD`（列表窄列用；解析失败原样返回） */
export function shortDate(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const match = /^(\d{4})-(\d{2})-(\d{2})/.exec(value);
  return match ? `${match[2]}-${match[3]}` : value;
}

/** 空值占位 */
export function dash(value: string | null | undefined): string {
  return value === null || value === undefined || value === '' ? '—' : value;
}

/** 短 id（主数据没加载完时兜底，别留白） */
export function shortId(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return value.length > 8 ? `${value.slice(0, 8)}…` : value;
}

/** 北京时间的今天（`YYYY-MM-DD`）—— 与后端 `today_cst()` 同口径 */
export function todayString(now: Date = new Date()): string {
  const shifted = new Date(now.getTime() + 8 * 60 * 60 * 1000);
  return shifted.toISOString().slice(0, 10);
}

/** 北京时间当前会计期间 `yyyy-MM`（总账 / 余额表默认期间） */
export function todayPeriod(now: Date = new Date()): string {
  return todayString(now).slice(0, 7);
}

/** `yyyy-MM` 是否合法（后端 `is_month_key` 同口径：4 位年 + `-` + 2 位月） */
export function isMonthKey(value: string): boolean {
  return /^\d{4}-(0[1-9]|1[0-2])$/.test(value);
}
