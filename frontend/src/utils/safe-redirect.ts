/**
 * 回跳地址的**安全校验**（防开放重定向）。
 *
 * # 为什么必须校验
 *
 * 空闲退出后要把用户送回原页面（卡面验收 ⑰），这个「原页面」来自
 * `sessionStorage.idle_logout_redirect`；登录页还会读 URL 上的 `?redirect=`。
 * 两处都是**可被篡改的输入**：`//evil.com` 是协议相对 URL，浏览器会直接跳到外站；
 * `http://evil.com` 同理。不校验就等于给攻击者一个「登录后跳钓鱼站」的入口。
 *
 * # 判定口径
 *
 * 只接受**站内相对路径**：以单个 `/` 开头、不含协议、不含反斜杠（部分浏览器把
 * `\` 归一成 `/`，`/\evil.com` 同样能出站）、不含控制字符。
 */

/** 默认回跳目标（工作台）。 */
export const DEFAULT_REDIRECT = '/dashboard';

/**
 * 校验并归一化回跳地址。
 *
 * @param raw 待校验的原始值（可能来自 URL query / sessionStorage / 任意脏值）
 * @param fallback 非法时的兜底目标，默认 {@link DEFAULT_REDIRECT}
 * @returns 合法的站内相对路径，或 `fallback`
 */
export function safeRedirect(raw: unknown, fallback: string = DEFAULT_REDIRECT): string {
  if (typeof raw !== 'string') {
    return fallback;
  }
  const value = raw.trim();
  if (!value) {
    return fallback;
  }
  // 必须是相对路径。这会挡掉 http://evil.com、javascript:alert(1)、evil.com
  if (!value.startsWith('/')) {
    return fallback;
  }
  // 协议相对 URL（//evil.com）——浏览器会当外站
  if (value.startsWith('//')) {
    return fallback;
  }
  // 显式协议（/a://b 这种畸形值一并拒绝）
  if (value.includes('://')) {
    return fallback;
  }
  // 反斜杠：`/\evil.com` 在部分浏览器等价 `//evil.com`
  if (value.includes('\\')) {
    return fallback;
  }
  // 控制字符（含 \t \n \r）：可被用来绕过前缀判断
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(value)) {
    return fallback;
  }
  return value;
}
