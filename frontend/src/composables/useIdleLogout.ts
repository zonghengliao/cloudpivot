/**
 * 空闲自动退出登录（AUTO-LOGOUT）的前端计时器。
 *
 * # 两段式行为（卡面口径）
 *
 * 1. **阶段 1**：距超时还剩 `warn_seconds`（后端下发，20–60 秒）时弹「您已 X 分钟
 *    无操作，将在 N 秒后自动退出」，按钮：继续使用 / 立即退出；
 * 2. **阶段 2**：到点自动退出 —— 调 `POST /api/system/auth/logout` **吊销令牌**
 *    （不是只清 localStorage）、记住当前路由、清前端状态、跳登录页并提示原因。
 *
 * # 为什么用事件节流 + setInterval，不用 requestIdleCallback
 *
 * `requestIdleCallback` 在后台标签页几乎不触发（Chrome 只在空闲期调度），用它计时
 * 「用户切走浏览器」这条最常见的场景恰好不成立。这里改成：DOM 事件节流写时间戳
 * （每 1 秒最多一次），`setInterval` 每 10 秒读一次时间戳做判定。后台标签页的
 * `setInterval` 会被节流到最低约 1 分钟一次 —— 对 15 分钟的窗口没有影响，反而
 * 更早发现不到，不会误杀。
 *
 * # 轮询不续期（卡面验收 ⑪）
 *
 * 计时只看 DOM 事件与路由变化，**不看请求**：首页未读数、看板刷新这些 GET 轮询
 * 不会把计时重置，所以「打开首页挂机」到点照样退出。
 *
 * # 多标签页（卡面验收 ⑨）
 *
 * - 时间戳落 localStorage，别的标签在操作时本标签也顺延（`storage` 事件同步）；
 * - 某标签退出时写广播键，其余标签收到后立即清状态跳登录页。
 */

import { onBeforeUnmount, ref, watch, type Ref } from 'vue';
import { useRouter } from 'vue-router';
import { fetchIdlePolicy } from '@/api/system/auth-api';
import { useUserStore } from '@/stores/user';
import {
  broadcastLogout,
  clearLogoutBroadcast,
  IDLE_ACTIVITY_KEY,
  IDLE_LOGOUT_BROADCAST_KEY,
  idleMillis,
  idlePhase,
  markActivity,
  resetActivity,
  saveIdleRedirect,
} from '@/utils/idle-activity';

/** 判定间隔（毫秒）。后台标签页会被浏览器节流到约 60 秒一次，不影响正确性。 */
const CHECK_INTERVAL_MS = 10_000;

/** 倒计时刷新间隔（毫秒）：弹窗出现后每秒更新剩余秒数。 */
const COUNTDOWN_INTERVAL_MS = 1_000;

/** 视为「用户操作」的 DOM 事件（capture 阶段监听，避免被业务层 stopPropagation 吞掉）。 */
const ACTIVITY_EVENTS = [
  'mousemove',
  'mousedown',
  'keydown',
  'wheel',
  'touchstart',
  'touchmove',
  'scroll',
  'click',
  'pointerdown',
] as const;

/** 退出原因：到点超时 / 用户主动点「立即退出」/ 其它标签要求退出。 */
export type IdleLogoutReason = 'timeout' | 'manual' | 'broadcast';

/** 计时器与控件状态（由 {@link useIdleLogout} 返回，供弹窗组件渲染）。 */
export interface IdleLogoutState {
  /** 当前是否启用（超管禁用后为 false） */
  enabled: Ref<boolean>;
  /** 生效的超时分钟数 */
  timeoutMinutes: Ref<number>;
  /** 「即将退出」提示是否可见 */
  warningVisible: Ref<boolean>;
  /** 提示文案里显示的「已无操作分钟数」 */
  idleMinutes: Ref<number>;
  /** 距离自动退出的剩余秒数 */
  remainingSeconds: Ref<number>;
  /** 点「继续使用」：重置计时并关闭提示 */
  keepAlive: () => void;
  /** 点「立即退出」：马上退出 */
  logoutNow: () => void;
  /** 启动监听（组件 mount 时调用；重复调用无副作用） */
  start: () => void;
  /** 停止监听（组件 unmount 时调用） */
  stop: () => void;
}

/**
 * 启动空闲计时。
 *
 * 必须由**单例组件**（`App.vue` 里的 `IdleLogoutGuard`）调用一次：它同时负责
 * 监听 DOM 事件、路由切换与跨标签广播，多挂几次会重复计时。
 */
export function useIdleLogout(): IdleLogoutState {
  const router = useRouter();
  const userStore = useUserStore();

  const enabled = ref(false);
  const timeoutMinutes = ref(0);
  const warningVisible = ref(false);
  const idleMinutes = ref(0);
  const remainingSeconds = ref(0);

  /** 提示窗口（秒）：后端按超时比例下发，0 表示禁用 */
  let warnSeconds = 0;
  /** 判定定时器 */
  let checkTimer: ReturnType<typeof setInterval> | null = null;
  /** 倒计时定时器（仅在提示可见时跑） */
  let countdownTimer: ReturnType<typeof setInterval> | null = null;
  /** 退出流程已开始：避免「定时器到点」与「用户点立即退出」并发两次 */
  let loggingOut = false;
  /** 是否已启动（防重复挂监听） */
  let started = false;
  /** 路由钩子注销函数（`router.afterEach` 的返回值） */
  let unregisterRouteHook: (() => void) | null = null;

  /** 当前是否处于登录态（没有令牌就不计时：登录页不该弹「即将退出」） */
  function isLoggedIn(): boolean {
    return Boolean(userStore.token || localStorage.getItem('x_access_token'));
  }

  /** 同步一次「已无操作分钟数」，供提示文案使用 */
  function syncIdleMinutes(): void {
    idleMinutes.value = Math.floor(idleMillis() / 60_000);
  }

  /** 打开提示并启动 1 秒倒计时 */
  function openWarning(): void {
    if (warningVisible.value) {
      return;
    }
    syncIdleMinutes();
    remainingSeconds.value = Math.max(0, Math.ceil((timeoutSeconds() - idleMillis() / 1000)));
    warningVisible.value = true;
    stopCountdown();
    countdownTimer = setInterval(() => {
      const remaining = Math.ceil((timeoutSeconds() - idleMillis() / 1000));
      remainingSeconds.value = Math.max(0, remaining);
      syncIdleMinutes();
      if (remainingSeconds.value <= 0) {
        void doLogout('timeout');
      }
    }, COUNTDOWN_INTERVAL_MS);
  }

  /** 关闭提示并停掉倒计时 */
  function closeWarning(): void {
    warningVisible.value = false;
    stopCountdown();
  }

  /** 生效窗口（秒） */
  function timeoutSeconds(): number {
    return timeoutMinutes.value * 60;
  }

  function stopCountdown(): void {
    if (countdownTimer !== null) {
      clearInterval(countdownTimer);
      countdownTimer = null;
    }
  }

  /**
   * 执行退出。
   *
   * 顺序刻意如此：
   * 1. 先记下当前路由（`sessionStorage`，登录后跳回）—— 必须在清状态之前取；
   * 2. 调后端 logout **吊销令牌**，这一步才是「真的退出」；失败也继续清本地；
   * 3. 广播给其它标签；
   * 4. 清前端状态并跳登录页（带 `reason`，登录页据此显示提示）。
   */
  async function doLogout(reason: IdleLogoutReason): Promise<void> {
    if (loggingOut) {
      return;
    }
    loggingOut = true;
    stopTimers();
    closeWarning();
    try {
      const current = router.currentRoute.value.fullPath;
      // 广播类退出由发起标签负责记路由，这里只在本地是活跃方时记录
      saveIdleRedirect(current);
      if (reason !== 'broadcast') {
        broadcastLogout();
        try {
          await userStore.logout();
        } catch {
          // 后端已把会话判超时（401 / 20009）时 logout 必然失败：本地照清
        }
      } else {
        userStore.reset();
      }
      clearLogoutBroadcast();
      await router.replace({ path: '/login', query: { reason: reason === 'manual' ? 'manual' : 'idle' } });
    } finally {
      loggingOut = false;
    }
  }

  /** 判定一次（每 10 秒） */
  function check(): void {
    if (!enabled.value || loggingOut) {
      return;
    }
    if (!isLoggedIn()) {
      closeWarning();
      return;
    }
    // 两段式判定集中在一个纯函数里（见 idlePhase）：active / warn / expired。
    // 这样「什么时候弹窗、什么时候真踢」能被 Node 单测钉死，不必起浏览器。
    const phase = idlePhase(idleMillis(), timeoutMinutes.value, warnSeconds);
    if (phase === 'expired') {
      void doLogout('timeout');
      return;
    }
    if (phase === 'warn') {
      openWarning();
    } else if (warningVisible.value) {
      // 用户点了「继续使用」或在别的标签里操作过 → 提示自动消失
      closeWarning();
    }
  }

  /** 任一时间源变化：内存 + 存储都刷新（跨标签由 storage 事件驱动） */
  function onActivity(): void {
    markActivity(Date.now());
  }

  /** 别的标签写了活动时间戳 / 退出广播 */
  function onStorage(event: StorageEvent): void {
    if (event.key === IDLE_ACTIVITY_KEY) {
      // 只同步内存：`markActivity(…, false)` 不写存储，避免来回打架
      const raw = Number(event.newValue);
      if (Number.isFinite(raw) && raw > 0) {
        markActivity(raw, false);
      }
      // 别的标签有人操作 → 本标签的提示也不该继续倒数
      if (warningVisible.value) {
        closeWarning();
      }
      return;
    }
    if (event.key === IDLE_LOGOUT_BROADCAST_KEY && event.newValue) {
      // 其它标签因空闲退出：本标签必须跟着退，否则「一个退了另一个还能点」
      void doLogout('broadcast');
    }
  }

  /** 路由变化也算用户操作（卡面：页面切换算「有操作」） */
  function onRouteChange(): void {
    onActivity();
  }

  /** 页面重新可见时不要自作主张重置计时 —— 用户可能只是切回来看一眼没动手 */
  function onVisibilityChange(): void {
    if (document.visibilityState === 'visible') {
      check();
    }
  }

  /** 拉取策略：超管把值设为 0（禁用）时前端整个不启动 */
  async function loadPolicy(): Promise<void> {
    try {
      const policy = await fetchIdlePolicy();
      timeoutMinutes.value = Math.max(0, policy.timeout_minutes);
      warnSeconds = Math.max(0, policy.warn_seconds);
      enabled.value = timeoutMinutes.value > 0;
      if (!enabled.value) {
        closeWarning();
      }
    } catch {
      // 拿不到策略时不猜：保持 enabled=false（不做前端计时）。
      // 服务端滑动过期仍然生效，绕过前端的调用会被 401 拦下。
      enabled.value = false;
    }
  }

  function stopTimers(): void {
    if (checkTimer !== null) {
      clearInterval(checkTimer);
      checkTimer = null;
    }
    stopCountdown();
  }

  function stop(): void {
    if (!started) {
      return;
    }
    started = false;
    stopTimers();
    ACTIVITY_EVENTS.forEach((name) => window.removeEventListener(name, onActivity, true));
    window.removeEventListener('storage', onStorage);
    document.removeEventListener('visibilitychange', onVisibilityChange);
    unregisterRouteHook?.();
    unregisterRouteHook = null;
  }

  function start(): void {
    if (started) {
      return;
    }
    started = true;
    // 启动即视为「刚刚有操作」：登录后立刻开始计时，而不是沿用上次会话的旧时间戳
    resetActivity();
    clearLogoutBroadcast();
    ACTIVITY_EVENTS.forEach((name) =>
      window.addEventListener(name, onActivity, { capture: true, passive: true }),
    );
    window.addEventListener('storage', onStorage);
    document.addEventListener('visibilitychange', onVisibilityChange);
    unregisterRouteHook = router.afterEach(() => onRouteChange());
    // 登录页（无令牌）不必拉策略：免得在未登录时打一个 401 出去
    if (isLoggedIn()) {
      void loadPolicy();
    }
    stopTimers();
    checkTimer = setInterval(check, CHECK_INTERVAL_MS);
    check();
  }

  // 登录状态变化时同步策略：
  // - 登录成功（token 出现）→ 拉生效值并重置计时起点；
  // - 退出（token 清空）→ 停表、关提示，不在登录页继续倒数。
  watch(
    () => userStore.token,
    (token) => {
      if (token) {
        resetActivity();
        void loadPolicy();
      } else {
        enabled.value = false;
        closeWarning();
      }
    },
  );

  onBeforeUnmount(stop);

  return {
    enabled,
    timeoutMinutes,
    warningVisible,
    idleMinutes,
    remainingSeconds,
    keepAlive: () => {
      resetActivity();
      closeWarning();
    },
    logoutNow: () => {
      void doLogout('manual');
    },
    start,
    stop,
  };
}
