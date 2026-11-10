import axios, { AxiosError, type AxiosInstance, type AxiosRequestConfig } from 'axios';
import { offerAssigneeFix } from '@/utils/flow-assignee-guard';
import {
  broadcastLogout,
  isRecentlyActive,
  saveIdleRedirect,
  USER_ACTIVE_HEADER,
} from '@/utils/idle-activity';
import { newRequestId, resolveRequestId, withRequestId, REQUEST_ID_HEADER } from './request-id';

// 请求 ID 的生成 / 解析 / 文案拼装都在 `./request-id`（纯逻辑，可被 Node 单测直接 import）；
// 此处 re-export 保持既有调用点 `import { newRequestId } from '@/api/request'` 不变。
export { REQUEST_ID_HEADER, newRequestId } from './request-id';

/** 后端统一响应体（对应 Rust 侧 `Res<T>`） */
export interface ApiResult<T = unknown> {
  /** 业务状态码，0 表示成功 */
  code: number;
  /** 提示消息 */
  msg: string;
  /** 业务数据 */
  data: T;
}

/** 分页数据体（对应后端 `Res::page` 的 data 字段） */
export interface PageData<T> {
  /** 当前页数据 */
  list: T[];
  /** 总记录数 */
  total: number;
  /** 当前页码，从 1 开始 */
  page: number;
  /** 每页条数 */
  page_size: number;
}

/** 访问令牌在 localStorage 中的键名 */
export const TOKEN_KEY = 'x_access_token';
/** 刷新令牌在 localStorage 中的键名 */
export const REFRESH_TOKEN_KEY = 'x_refresh_token';

/** 接口基础路径，与 axios 实例保持一致 */
const BASE_URL = import.meta.env.VITE_APP_BASE_API || '';

/** 刷新令牌接口路径（免登录，401 时自动调用） */
const REFRESH_PATH = '/api/system/auth/refresh';

/**
 * 后端错误码：会话因长时间未操作已自动退出（`AUTH_IDLE_TIMEOUT`，见
 * `backend/src/shared/error_code.rs`）。
 *
 * 这类 401 **不能走刷新流程**：服务端在判超时的同时已经吊销了该会话名下的 refresh
 * 令牌（`revoke_refresh_by_sid`），刷新必然失败；而且用户要看到的是「因长时间未操作
 * 已自动退出」而不是「登录状态已失效」。
 */
const AUTH_IDLE_TIMEOUT_CODE = 20009;

/** 登录页路径，令牌彻底失效后跳转至此 */
const LOGIN_PATH = `${import.meta.env.VITE_APP_CONTEXT_PATH || '/'}login`.replace(/\/{2,}/g, '/');

/** 刷新令牌接口的返回体（仅取本模块所需字段，避免与 api/auth 循环依赖） */
interface RefreshPayload {
  /** 新的访问令牌 */
  access_token: string;
  /** 新的刷新令牌 */
  refresh_token: string;
}

/** 支持「401 后重放一次」的请求配置，标记位用于防止无限重试 */
type RetriableConfig = AxiosRequestConfig & { /** 是否已经因 401 重放过 */ _retried?: boolean };

/**
 * 业务异常：后端返回了 `code !== 0` 的统一响应体，或 HTTP 层请求失败。
 *
 * 调用方通过 `instanceof ApiError` 区分「业务失败」与「代码缺陷」。
 *
 * # requestId（N-4）
 *
 * `requestId` 是**该次请求**的服务端 ID，与 `journald` 日志里 `request{request_id=…}`、
 * 响应头 `X-Request-Id`、`operation_logs.request_id` 是同一个值。用户报障时凭它一条命令
 * 就能还原整条链路：
 *
 * ```bash
 * journalctl -u cloudpivot-base --since -1h | grep <requestId>
 * ```
 *
 * 为了让仓内 175 处既有的 `showError(err.message)` 不用逐个改就带上它，构造时会把
 * ID 追加到 `message` 末尾（`…（请求 ID：xxx）`）。需要纯文案时读 `requestId` 字段。
 */
export class ApiError extends Error {
  /** 业务状态码；网络层失败时为 0 */
  readonly code: number;
  /**
   * HTTP 状态码（`undefined` = 网络层失败 / 超时 / 没拿到响应）。
   *
   * 为什么需要它：**业务「不存在」的码按域散落**（任务 `63001`、合同 `66014`、
   * 盘点/边配置 `65005` …），而且新增业务类型还会继续加码 —— 调用方要判
   * 「这条记录没了」只能看 HTTP 404，不能背一张越来越长的码表。
   * 可选字段：老调用方一个都不用改。
   */
  readonly status?: number;
  /**
   * 错误体里可选的结构化载荷（后端 `ApiError::with_data` 写的那份 JSON）。
   *
   * 为什么错误也要带 data：`65018`（无可用审批人）要让前端弹「给哪个角色指派审批人」
   * 的引导框，需要节点 / 角色 / 成员数这类字段；解析中文 `msg` 是脆的（后端改一个字
   * 前端就崩）。没有载荷的错误码（绝大多数）此字段为 `undefined`。
   */
  readonly data?: unknown;
  /** 本次请求的服务端 `request_id`（用于 grep 日志还原链路；无请求上下文时为 `undefined`） */
  readonly requestId?: string;

  constructor(code: number, message: string, status?: number, data?: unknown, requestId?: string) {
    // 追加而非替换：既有 175 处 `showError(err.message)` 一个都不用改就能带上 ID
    super(withRequestId(message, requestId));
    this.name = 'ApiError';
    this.code = code;
    this.status = status;
    this.data = data;
    this.requestId = requestId;
  }
}

const service: AxiosInstance = axios.create({
  baseURL: BASE_URL,
  timeout: 15000,
});

// 请求拦截：注入 Bearer Token + 每个请求一个 X-Request-Id
// （后端会原样采信并写进日志 / 响应头，两侧因此能用同一个 ID 排查同一次请求）
service.interceptors.request.use((config) => {
  const token = localStorage.getItem(TOKEN_KEY);
  if (token) {
    config.headers.Authorization = `Bearer ${token}`;
  }
  // 已经显式指定过（例如重放时保留原 ID）就不覆盖
  if (!config.headers[REQUEST_ID_HEADER]) {
    config.headers[REQUEST_ID_HEADER] = newRequestId();
  }
  // 空闲超时（AUTO-LOGOUT）：只有「刚刚有真实操作」的请求才打标，后端据此续期。
  // 首页未读数 / 看板刷新这类自动轮询**不打标** —— 否则打开首页挂机永远不超时
  // （卡面验收 ⑪）。判定只看 DOM 事件与路由，不看请求本身。
  config.headers[USER_ACTIVE_HEADER] = isRecentlyActive() ? '1' : '0';
  return config;
});

// 响应拦截：401 时用 refresh_token 换新令牌并重放原请求，其余错误规整成 ApiError
service.interceptors.response.use(
  (response) => response,
  async (error: AxiosError<ApiResult>) => {
    const config = error.config as RetriableConfig | undefined;
    // 空闲超时（401 + 20009）：服务端已判定本会话长时间未操作，refresh 会话也已被
    // 吊销。这里不再尝试刷新，直接记下当场路由、广播给其它标签、清令牌回登录页
    // （带 reason=idle，登录页据此显示「因长时间未操作已自动退出」）。
    if (isIdleTimeout(error)) {
      saveIdleRedirect(`${window.location.pathname}${window.location.search}${window.location.hash}`);
      broadcastLogout();
      clearTokens();
      redirectToLogin('idle');
      return Promise.reject(toApiError(error));
    }
    if (shouldRefresh(error, config)) {
      // 标记已重放，避免刷新成功但接口仍返回 401 时陷入死循环
      config._retried = true;
      const accessToken = await refreshAccessToken();
      if (accessToken) {
        const headers = config.headers ?? {};
        headers.Authorization = `Bearer ${accessToken}`;
        config.headers = headers;
        return service.request(config);
      }
      redirectToLogin();
    }
    const apiError = toApiError(error);
    // FIX-1：`65018`（无可用审批人）→ 弹「指派审批人」引导框；指派成功后**自动重试原请求**。
    // 放在拦截器里（而不是每个提交按钮各接一遍）是因为「提交审批」的入口散布在
    // 销售单 / 采购单 / 报销 / 预算 / 盘点 / 处置… 十几个页面，逐个接必然漏。
    if (await offerAssigneeFix(apiError, config?.url)) {
      return service.request(config as AxiosRequestConfig);
    }
    return Promise.reject(apiError);
  },
);

/** 判断当前失败是否需要走「刷新令牌 + 重放」流程 */
function shouldRefresh(error: AxiosError<ApiResult>, config?: RetriableConfig): config is RetriableConfig {
  if (error.response?.status !== 401 || !config || config._retried) {
    return false;
  }
  // 刷新接口自身返回 401 时不再递归刷新
  return config.url !== REFRESH_PATH;
}

/**
 * 是否是「会话空闲超时」401（`code = 20009`）。
 *
 * 与普通 401 的处置不同：不刷新、不清缓存就跳登录，而是要带上「因长时间未操作已自动
 * 退出」的提示，并把当前路由留给登录后跳回（卡面验收 ⑥⑦⑰）。
 */
function isIdleTimeout(error: AxiosError<ApiResult>): boolean {
  if (error.response?.status !== 401) {
    return false;
  }
  const body = error.response.data;
  return Boolean(body && typeof body === 'object' && body.code === AUTH_IDLE_TIMEOUT_CODE);
}

/** 进行中的刷新请求，用于把并发 401 合并成一次刷新 */
let refreshing: Promise<string | null> | null = null;

/**
 * 用本地 refresh_token 换取新的访问令牌。
 *
 * # 返回值
 *
 * 刷新成功返回新的 access_token；无 refresh_token 或刷新失败返回 null（并清空本地令牌）。
 */
function refreshAccessToken(): Promise<string | null> {
  refreshing ??= doRefresh().finally(() => {
    refreshing = null;
  });
  return refreshing;
}

/** 真正执行刷新请求；此处用裸 axios，避免复用带拦截器的实例造成递归 */
async function doRefresh(): Promise<string | null> {
  const refreshToken = localStorage.getItem(REFRESH_TOKEN_KEY);
  if (!refreshToken) {
    clearTokens();
    return null;
  }
  try {
    const response = await axios.post<ApiResult<RefreshPayload>>(
      REFRESH_PATH,
      { refresh_token: refreshToken },
      { baseURL: BASE_URL, timeout: 15000 },
    );
    const body = response.data;
    if (!body || body.code !== 0 || !body.data?.access_token) {
      clearTokens();
      return null;
    }
    saveTokens(body.data.access_token, body.data.refresh_token);
    return body.data.access_token;
  } catch {
    clearTokens();
    return null;
  }
}

/**
 * 令牌彻底失效时跳转登录页（replace 避免把失效页面留在历史记录里）。
 *
 * @param reason 登录页展示的提示原因：`idle` = 因长时间未操作已自动退出
 */
function redirectToLogin(reason?: 'idle'): void {
  if (window.location.pathname.endsWith('/login')) {
    return;
  }
  window.location.replace(reason ? `${LOGIN_PATH}?reason=${reason}` : LOGIN_PATH);
}

/** 把 axios 的各类失败（网络、超时、非 2xx）转换为带可读消息的 ApiError */
function toApiError(error: AxiosError<ApiResult>): ApiError {
  const body = error.response?.data;
  const status = error.response?.status;
  // 后端回传的 x-request-id 优先；网络层失败时退回我们自己发出去的那个
  const requestId = resolveRequestId(error.response?.headers, error.config?.headers?.[REQUEST_ID_HEADER]);
  // 后端在 401/403/500 等错误路径上也返回统一响应体，优先采用其 msg
  if (body && typeof body === 'object' && typeof body.msg === 'string' && body.msg) {
    if (status === 401) {
      clearTokens();
    }
    return new ApiError(
      typeof body.code === 'number' ? body.code : -1,
      body.msg,
      status,
      // 65018 这类错误带结构化载荷（见 ApiError.data 的注释）；其它错误为 undefined
      (body as { data?: unknown }).data,
      requestId,
    );
  }
  if (status === 401) {
    clearTokens();
    return new ApiError(-1, '登录状态已失效，请重新登录', status, undefined, requestId);
  }
  if (status === 403) {
    return new ApiError(-1, '权限不足', status, undefined, requestId);
  }
  if (error.code === 'ECONNABORTED') {
    return new ApiError(-1, '请求超时，请稍后重试', undefined, undefined, requestId);
  }
  return new ApiError(-1, error.message || '网络异常，请检查服务是否可用', undefined, undefined, requestId);
}

/** 清除本地令牌（登出或令牌失效时调用） */
export function clearTokens(): void {
  localStorage.removeItem(TOKEN_KEY);
  localStorage.removeItem(REFRESH_TOKEN_KEY);
}

/** 保存令牌对 */
export function saveTokens(accessToken: string, refreshToken?: string): void {
  localStorage.setItem(TOKEN_KEY, accessToken);
  if (refreshToken) {
    localStorage.setItem(REFRESH_TOKEN_KEY, refreshToken);
  }
}

/**
 * 发起请求并拆解统一响应体。
 *
 * # 返回值
 *
 * 成功时返回后端 `data` 字段的内容；`code !== 0` 或 HTTP 失败时抛出 {@link ApiError}。
 */
async function request<T>(config: AxiosRequestConfig): Promise<T> {
  const response = await service.request<ApiResult<T>>(config);
  const body = response.data;
  // HTTP 200 但业务码非 0 时也要带上 request_id（后端此时也会把 ID 放进响应头）
  const requestId = resolveRequestId(
    response.headers,
    (response.config.headers as Record<string, unknown> | undefined)?.[REQUEST_ID_HEADER],
  );
  if (body === null || typeof body !== 'object' || typeof body.code !== 'number') {
    throw new ApiError(-1, '响应体格式不符合统一响应规范', response.status, undefined, requestId);
  }
  if (body.code !== 0) {
    throw new ApiError(body.code, body.msg, response.status, (body as { data?: unknown }).data, requestId);
  }
  return body.data;
}

/** GET 请求 */
export function get<T>(url: string, params?: Record<string, unknown>): Promise<T> {
  return request<T>({ url, method: 'GET', params });
}

/** POST 请求 */
export function post<T>(url: string, data?: unknown): Promise<T> {
  return request<T>({ url, method: 'POST', data });
}

/** PUT 请求 */
export function put<T>(url: string, data?: unknown): Promise<T> {
  return request<T>({ url, method: 'PUT', data });
}

/** PATCH 请求（部分更新） */
export function patch<T>(url: string, data?: unknown): Promise<T> {
  return request<T>({ url, method: 'PATCH', data });
}

/** DELETE 请求 */
export function del<T>(url: string, params?: Record<string, unknown>): Promise<T> {
  return request<T>({ url, method: 'DELETE', params });
}

export default service;
