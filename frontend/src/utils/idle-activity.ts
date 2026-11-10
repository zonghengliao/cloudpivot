/**
 * 空闲（自动退出登录）的**纯状态**：最后一次用户操作时间 + 跨标签页广播。
 *
 * 抽成无框架依赖的模块有三个理由：
 * 1. `api/request.ts` 要在请求头上标「本次请求由用户操作触发」，而 `request.ts` 位于
 *    依赖图底部，不能反过来 import store / 组件；
 * 2. 单测跑在 Node（`tsx`）下，import 任何 `@/api/*` 都会因顶层 `import.meta.env` 抛错；
 * 3. 「什么算用户操作」是产品口径（见卡面 AUTO-LOGOUT），必须只有一个判定点，
 *    否则前端计时与请求打标会各说各话。
 *
 * # 什么算「有操作」
 *
 * 只有**真实输入**：鼠标移动 / 按下 / 滚轮、键盘、触摸、页面切换（路由变化）。
 * 后台轮询（首页未读数、看板自动刷新）**不算** —— 判定只看 DOM 事件与路由，
 * 不看请求，所以轮询永远不会把计时重置。
 *
 * # 多标签页
 *
 * 时间戳落在 `localStorage`（跨标签共享），并用 `storage` 事件把「别的标签有人在操作」
 * 同步过来；某标签判定退出时写一个广播键，其余标签收到后立即跟着退出 ——
 * 否则「一个标签退了、另一个还能点」就是假退出。
 */

/** localStorage 键：最后一次用户操作的时间戳（毫秒），跨标签共享。 */
export const IDLE_ACTIVITY_KEY = 'idle:last-active-at';

/** localStorage 键：某标签已因空闲退出（值为毫秒时间戳），其余标签据此跟随退出。 */
export const IDLE_LOGOUT_BROADCAST_KEY = 'idle:logout-broadcast-at';

/** sessionStorage 键：空闲退出前所在路由，重新登录后跳回（关标签即失效）。 */
export const IDLE_REDIRECT_KEY = 'idle_logout_redirect';

/** 请求头名：本次请求由用户操作触发（后端据此刷新空闲计时）。 */
export const USER_ACTIVE_HEADER = 'X-User-Active';

/** 多久之内发生过操作，才给请求打 `X-User-Active: 1`（毫秒）。 */
export const ACTIVE_REQUEST_WINDOW_MS = 5_000;

/** 高频事件的落盘节流窗口（毫秒）：鼠标移动每 1 秒最多写一次存储。 */
const ACTIVITY_PERSIST_THROTTLE_MS = 1_000;

/** 本进程内最后一次操作时刻（毫秒）；`0` 表示本次页面生命周期内还没有记录。 */
let lastActivityAt = 0;

/** 上次把时间戳写进 localStorage 的时刻（节流用）。 */
let lastPersistedAt = 0;

/** 取浏览器存储；非浏览器环境（Node 单测 / SSR）返回 `null`。 */
function safeStorage(kind: 'local' | 'session'): Storage | null {
  try {
    if (typeof window === 'undefined') {
      return null;
    }
    return kind === 'local' ? window.localStorage : window.sessionStorage;
  } catch {
    // 隐私模式 / 存储被禁用：降级为纯内存计时，不影响正常退出
    return null;
  }
}

/** 读取本地存储里别处写入的时间戳（解析失败按 0 处理）。 */
function readTimestamp(key: string): number {
  const storage = safeStorage('local');
  if (!storage) {
    return 0;
  }
  const raw = storage.getItem(key);
  if (!raw) {
    return 0;
  }
  const value = Number(raw);
  return Number.isFinite(value) ? value : 0;
}

/**
 * 记录一次「用户操作」。
 *
 * @param at 操作时刻（毫秒），默认取当前时间；单测可注入固定值
 * @param persist 是否落盘（跨标签共享）；高频事件走节流，`false` 时只更新内存
 */
export function markActivity(at: number = Date.now(), persist = true): void {
  lastActivityAt = at;
  if (!persist) {
    return;
  }
  // 节流：滚动 / 鼠标移动每秒可触发几十次，逐次写存储会拖慢主线程
  if (at - lastPersistedAt < ACTIVITY_PERSIST_THROTTLE_MS && at >= lastPersistedAt) {
    return;
  }
  lastPersistedAt = at;
  safeStorage('local')?.setItem(IDLE_ACTIVITY_KEY, String(at));
}

/** 强制把当前时刻落盘（用于「继续使用」按钮与登录成功后的重置）。 */
export function resetActivity(at: number = Date.now()): void {
  lastPersistedAt = 0;
  markActivity(at, true);
}

/**
 * 最近一次用户操作时刻（毫秒）：内存值与存储值取较大者。
 *
 * 取较大者的原因：别的标签刚有操作时，本标签内存里的值更旧，若只看内存会把
 * 「正在另一个标签里干活」的用户踢下线。
 *
 * @param storedFallback 无任何记录时的兜底值（默认当前时间，即「从现在开始计时」）
 */
export function lastActivity(storedFallback: number = Date.now()): number {
  const stored = readTimestamp(IDLE_ACTIVITY_KEY);
  const latest = Math.max(lastActivityAt, stored);
  return latest > 0 ? latest : storedFallback;
}

/**
 * 距上次用户操作过了多少毫秒。
 *
 * @param at 参考时刻（毫秒）
 */
export function idleMillis(at: number = Date.now()): number {
  return Math.max(0, at - lastActivity(at));
}

/**
 * 是否「刚刚有操作」——决定本次请求是否带 `X-User-Active: 1`。
 *
 * 窗口取 {@link ACTIVE_REQUEST_WINDOW_MS}：用户在页面上有点击 / 输入后 5 秒内的
 * 请求才续期。若把窗口放很大，后台轮询会被一并续期，超时就永远不触发。
 */
export function isRecentlyActive(at: number = Date.now()): boolean {
  return idleMillis(at) <= ACTIVE_REQUEST_WINDOW_MS;
}

/**
 * 空闲判定的三段状态（两段式超时的判定核心，纯函数）。
 *
 * - `'active'`：还没到提示窗口，正常使用；
 * - `'warn'`：已进入「即将退出」窗口（距超时 ≤ `warnSeconds`），应弹提示并可「继续使用」；
 * - `'expired'`：已到点，必须退出（调 logout 吊销令牌）。
 *
 * 抽成纯函数的理由：这条判定决定「什么时候弹窗 / 什么时候真踢人」，
 * 错一次就是「正常干活被踢」或「该踢不踢」。放这里才能被 Node 单测钉死，
 * 不必起浏览器。
 *
 * @param idleMs 距上次用户操作的毫秒数
 * @param timeoutMinutes 生效的超时分钟数；`0` = 禁用（永远 `active`）
 * @param warnSeconds 提前提示窗口（秒）；`0` = 不提示、到点直接过期
 */
export function idlePhase(
  idleMs: number,
  timeoutMinutes: number,
  warnSeconds: number,
): 'active' | 'warn' | 'expired' {
  if (!Number.isFinite(timeoutMinutes) || timeoutMinutes <= 0) {
    return 'active';
  }
  const totalMs = timeoutMinutes * 60_000;
  const idle = Number.isFinite(idleMs) ? Math.max(0, idleMs) : 0;
  if (idle >= totalMs) {
    return 'expired';
  }
  const warnMs = Math.max(0, warnSeconds) * 1000;
  if (warnMs > 0 && idle >= totalMs - warnMs) {
    return 'warn';
  }
  return 'active';
}

/** 广播「本标签已因空闲退出」，让其它标签一起退。 */
export function broadcastLogout(at: number = Date.now()): void {
  safeStorage('local')?.setItem(IDLE_LOGOUT_BROADCAST_KEY, String(at));
}

/** 读取退出广播的时间戳（`0` = 没有广播）。 */
export function readLogoutBroadcast(): number {
  return readTimestamp(IDLE_LOGOUT_BROADCAST_KEY);
}

/** 清掉退出广播（新一轮登录后调用，避免旧广播误伤新会话）。 */
export function clearLogoutBroadcast(): void {
  safeStorage('local')?.removeItem(IDLE_LOGOUT_BROADCAST_KEY);
}

/** 记住「空闲退出前在哪」（`sessionStorage`：关标签即失效，不留脏数据）。 */
export function saveIdleRedirect(fullPath: string): void {
  if (!fullPath || fullPath.startsWith('/login')) {
    return;
  }
  safeStorage('session')?.setItem(IDLE_REDIRECT_KEY, fullPath);
}

/** 取出并清空「空闲退出前在哪」（登录成功后调用一次）。 */
export function takeIdleRedirect(): string {
  const storage = safeStorage('session');
  if (!storage) {
    return '';
  }
  const value = storage.getItem(IDLE_REDIRECT_KEY) ?? '';
  storage.removeItem(IDLE_REDIRECT_KEY);
  return value;
}
