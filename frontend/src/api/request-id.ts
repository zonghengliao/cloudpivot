/**
 * 请求 ID 贯穿（前端侧的纯逻辑，N-4）。
 *
 * # 为什么单独成文件
 *
 * `api/request.ts` 在模块顶层读 `import.meta.env`（Vite 专有），任何 Node 侧单测都无法
 * `import` 它；把「生成 / 解析 / 拼进文案」这三段纯逻辑抽出来，就可以被
 * `tests/unit/n4-request-id.test.ts` 真跑（而不是只做源码文本断言）。
 *
 * # 与后端的契约
 *
 * - 头名 `X-Request-Id`（后端 `middleware::request_context::REQUEST_ID_HEADER`）；
 * - 后端只采信「≤128 字节、只含 `[A-Za-z0-9._:-]`」的值，不合法会换成它自己生成的 UUID v7；
 * - 后端一定在响应头回传最终 ID —— 前端展示/报障应当用**响应头那个**（它才在服务端日志里）。
 */

/** 请求 ID 头名（与后端 `x-request-id` 同名） */
export const REQUEST_ID_HEADER = 'X-Request-Id';

/** 后端 `MAX_INCOMING_LEN` 的对齐值：超过就不必发了（发了也会被丢弃） */
export const MAX_REQUEST_ID_LEN = 128;

/** 后端采信的字符集（`[A-Za-z0-9._:-]`） */
const ACCEPTABLE = /^[A-Za-z0-9._:-]{1,128}$/;

/**
 * 生成本次请求的 `request_id`。
 *
 * 优先 `crypto.randomUUID()`（安全上下文：https / localhost）。内网 http 部署下该 API
 * **不存在**，退化为 `web-<时间戳36进制>-<随机串>`（同属可采信字符集）。
 */
export function newRequestId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) {
    return uuid;
  }
  return `web-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * 判断一个值是否是后端会采信的 `request_id`。
 *
 * 用途：重放 / 重试时复用旧 ID 前先判定，避免把超长或含控制字符的值再发一遍。
 */
export function isAcceptableRequestId(value: unknown): value is string {
  return typeof value === 'string' && ACCEPTABLE.test(value);
}

/**
 * 从响应头 / 请求配置里取本次请求的 `request_id`。
 *
 * 优先级：后端回传的 `x-request-id`（唯一可在服务端日志里 grep 到的值）→ 我们自己发出去的
 * 那个（超时 / 连接被重置时后端可能已处理，仍值得让用户报上来）。
 *
 * @param headers 响应头（axios 已把键名小写化；网络层失败时没有响应）
 * @param sent    请求拦截器写进配置的那个值
 * @returns 可展示的请求 ID；两者都没有时返回 `undefined`
 */
export function resolveRequestId(headers: unknown, sent?: unknown): string | undefined {
  const fromResponse = (headers as Record<string, unknown> | undefined)?.['x-request-id'];
  if (isAcceptableRequestId(fromResponse)) {
    return fromResponse;
  }
  if (typeof fromResponse === 'string' && fromResponse.trim()) {
    return fromResponse.trim();
  }
  return typeof sent === 'string' && sent.trim() ? sent.trim() : undefined;
}

/**
 * 把请求 ID 追加到用户可见文案末尾（`…（请求 ID：xxx）`）。
 *
 * 为什么追加到 `message` 而不是只留字段：仓内有 175 处 `showError(err.message)`，
 * 逐个改必漏；追加后**所有**既有提示自动带上可报障的 ID，且历史 `includes()` 判定不受影响。
 */
export function withRequestId(message: string, requestId?: string): string {
  return requestId ? `${message}（请求 ID：${requestId}）` : message;
}
