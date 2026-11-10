/**
 * 成本三页共用的定点小数工具（**纯函数，不 import 请求层**）。
 *
 * # 为什么需要它
 *
 * 后端金额 / 工时 / 基数出参一律是**字符串**（`"3000.0000"` / `"8.00"` / `"0.0001"`），
 * 前端还要做三类运算：
 *
 * 1. **合计**（按成本对象分组的人工工时、费用池 / 直接金额、试算池与分摊额）；
 * 2. **守恒判定**（Σ分摊额 == 池金额，字面比较会因位数不同而抖）；
 * 3. **精度校验**（提交前拦"8.555 小时"，别让库把列值悄悄舍成 8.56）。
 *
 * `"8.00" + "2.00"` 是字符串拼接（`"8.002.00"`），`Number` 相加又会在 `0.1 + 0.2`
 * 处露馅，所以统一走**整数定点**：按标度换成整数相加，再格式化回字符串。
 *
 * # 口径
 *
 * - 超过标度的小数位**截断**（不做四舍五入）—— 展示用；真正的入账舍入在服务端；
 * - 尾零不算超精度（`8.500` 归一后是 1 位小数，与后端 `ensure_decimal` 同口径）。
 */

/** 小数文本是否合法且不超过 `scale` 位小数（尾零不算超精度） */
export function isDecimalWithinScale(value: string, scale: number): boolean {
  const text = value.trim();
  if (!/^-?\d+(\.\d+)?$/.test(text)) {
    return false;
  }
  const [, fraction = ''] = text.split('.');
  return fraction.replace(/0+$/, '').length <= scale;
}

/** 字符串小数 → 定点整数（截断多余小数位；非法输入当 0） */
export function scaledInt(value: string | null | undefined, scale: number): number {
  if (value === null || value === undefined) {
    return 0;
  }
  const text = String(value).trim();
  const match = /^(-?)(\d+)(?:\.(\d*))?$/.exec(text);
  if (!match) {
    return 0;
  }
  const [, sign, intPart, rawFraction = ''] = match;
  const fraction = rawFraction.padEnd(scale, '0').slice(0, scale);
  const combined = Number(`${intPart}${fraction}`);
  if (!Number.isFinite(combined)) {
    return 0;
  }
  return sign === '-' ? -combined : combined;
}

/** 定点整数 → 字符串小数（固定 `scale` 位） */
export function formatScaled(value: number, scale: number): string {
  const negative = value < 0;
  const abs = Math.abs(Math.trunc(value));
  const text = String(abs).padStart(scale + 1, '0');
  const intPart = text.slice(0, text.length - scale);
  const fraction = scale > 0 ? `.${text.slice(text.length - scale)}` : '';
  return `${negative ? '-' : ''}${intPart}${fraction}`;
}

/**
 * 定点累加（字符串入、字符串出，按 `scale` 位小数截断）。
 *
 * `sumDecimal(['8.00', '2.5'], 2) === '10.50'`。空串 / null 当 0。
 */
export function sumDecimal(values: Array<string | null | undefined>, scale: number): string {
  let total = 0;
  for (const value of values) {
    total += scaledInt(value, scale);
  }
  return formatScaled(total, scale);
}

/**
 * 两个字符串小数是否**数值相等**（位数不同也算等：`"2400.00"` == `"2400.0000"`）。
 *
 * 试算的守恒判定用它（Σ分摊额 == 池），字面比较会因为后端固定 4 位 / 2 位而抖。
 */
export function decimalEquals(left: string, right: string, scale = 4): boolean {
  return scaledInt(left, scale) === scaledInt(right, scale);
}

/**
 * 占比（基数值 / 基数合计，0~1 的 number；合计为 0 时返回 0）。
 *
 * 只用于展示进度条 / 百分比，不参与任何金额计算。
 */
export function ratio(basis: string, total: string): number {
  const basisInt = scaledInt(basis, 4);
  const totalInt = scaledInt(total, 4);
  if (totalInt === 0) {
    return 0;
  }
  const value = basisInt / totalInt;
  if (!Number.isFinite(value)) {
    return 0;
  }
  return Math.min(1, Math.max(0, value));
}
