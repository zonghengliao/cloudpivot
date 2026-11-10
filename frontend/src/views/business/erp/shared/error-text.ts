/**
 * 接口错误 → 人话（成本三页共用）。
 *
 * # 为什么单独一个文件
 *
 * `cost-labor` / `cost-overhead` / `cost-alloc-rule` 三页都要把后端 `ApiError`
 * （`{ code, message }`，`message` 里已带请求 ID 与「怎么修」）展示到界面上。
 * 每个页面各抄一份 `errorText` 必然漂移（有的吞掉 `code`，有的把 `[object Object]` 打出来），
 * 所以收在这里；它是**纯函数**（不 import 请求层），`tsx --test` 可直接单测。
 *
 * # 口径
 *
 * 1. `ApiError.message` 是后端逐条写好的人话（含"怎么修"），**原样展示**，不做中文字符串解析；
 * 2. 只有 `message` 缺失时才退化成 `业务码 N`（比"请求失败"更能定位问题）；
 * 3. 业务码单独暴露（`errorCodeOf`），页面据此区分「硬错误要不要换引导文案」。
 */

/** 后端错误对象的最小形状（`@/api/request` 的 `ApiError` 满足它） */
export interface ErrorLike {
  /** HTTP / 业务状态码（如 `66062`） */
  code?: number;
  /** 人话报文 */
  message?: string;
  /** 后端 JSON 里偶尔叫 `msg` */
  msg?: string;
}

/** 取错误对象上的业务码（取不到 → `null`） */
export function errorCodeOf(error: unknown): number | null {
  if (error && typeof error === 'object') {
    const code = (error as ErrorLike).code;
    if (typeof code === 'number' && Number.isFinite(code)) {
      return code;
    }
  }
  return null;
}

/**
 * 错误 → 展示文案。
 *
 * `prefix` 用于拼"加载失败：xxx"这类前缀（缺省不带）。
 */
export function errorText(error: unknown, prefix = ''): string {
  const head = prefix ? `${prefix}：` : '';
  if (typeof error === 'string') {
    const text = error.trim();
    return text ? `${head}${text}` : `${head}请求失败，请重试`;
  }
  if (error && typeof error === 'object') {
    const like = error as ErrorLike;
    const message = (like.message ?? like.msg ?? '').trim();
    if (message) {
      return `${head}${message}`;
    }
    const code = errorCodeOf(error);
    if (code !== null) {
      return `${head}请求失败（业务码 ${code}）`;
    }
  }
  return `${head}请求失败，请重试`;
}
