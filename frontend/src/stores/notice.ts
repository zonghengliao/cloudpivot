import { ref } from 'vue';
import { defineStore } from 'pinia';
import {
  buildNoticeStreamUrl,
  fetchUnreadCount,
  pageMyNotices,
} from '@/api/business/notice';
import { TOKEN_KEY } from '@/api/request';
import {
  filterArrivedSince,
  MAX_POPUP_ITEMS,
  NoticeAccumulator,
  type NoticePopupItem,
} from '@/utils/notice-popup';
import { openNoticeStream, type SseFrame } from '@/utils/notice-stream';
import { useUserStore } from '@/stores/user';
import { PERM } from '@/constants/permissions';

/** 未读数轮询间隔（实时通道不可用时的兜底；30 秒足够，不给后端添压力） */
const POLL_INTERVAL_MS = 30_000;

/**
 * 消息通知状态：未读数、实时连接、「最新消息」（右上角自动浮层的数据源）。
 *
 * # 生命周期
 *
 * 由 `BasicLayout` 在挂载时 `connect()`、卸载时 `disconnect()`，全局只有一条连接；
 * 消息中心页面复用同一份未读数，避免顶栏与页面各拉一次导致角标不同步。
 *
 * # 两条到达通道（都汇到 `handleFrame` / `pullLatest`）
 *
 * 1. **`fetch` + `ReadableStream`**（带 `Authorization` 头）—— 唯一的实时通道。
 *    浏览器原生 `EventSource` 不能设置请求头，只能退化成 `?token=`，而认证层只对
 *    `[middleware.auth].query_token_paths` 白名单里的端点采信查询令牌：一旦某个环境
 *    漏配，`EventSource` 就**每次页面加载都刷一条 `401`**、并自动重连继续刷
 *    （P1-FIX-6 的根因）。这里改用带头流 —— 它与普通接口走同一套认证，**不依赖**白名单；
 * 2. **轮询**（30 秒）—— 实时通道断开时，靠「未读数上涨」判定有新消息，再拉最新 5 条。
 *
 * 两条通道可能**同时**送来同一条消息，所以入队前按 `receiver_id` 去重（`seenReceiverIds`）。
 */
export const useNoticeStore = defineStore('notice', () => {
  /** 未读条数（顶栏角标与消息中心共用） */
  const unreadCount = ref(0);
  /** 是否已建立实时连接（SSE 或带头流任一活着） */
  const connected = ref(false);
  /** 收到推送时自增，供页面 watch 后刷新列表 */
  const refreshToken = ref(0);

  /** 最近收到的消息（新的在前，最多 5 条）—— 右上角浮层的数据源 */
  const latestItems = ref<NoticePopupItem[]>([]);
  /** 本批新消息条数（浮层标题「新消息 N 条」；浮层关闭后归零） */
  const newBatchCount = ref(0);
  /** 每入队一条就 +1；浮层 watch 它决定「有新消息 → 弹」 */
  const popupSeq = ref(0);

  let pollTimer: number | undefined;
  let streamAbort: AbortController | null = null;
  let streamRetryTimer: number | undefined;
  let streamRunning = false;
  /** 上一次已知的未读数（`null` = 还没拉到过，首次拉取只建基线不算「上涨」） */
  let lastKnownUnread: number | null = null;
  /** 「最新消息」累加器：去重 / 新的在前 / 上限 5 条（规则与单测都在 utils/notice-popup.ts） */
  const accumulator = new NoticeAccumulator();

  /**
   * 当前账号是否有站内信权限（`business:notice:mine`，与后端 `#[nik::permission]` 逐字一致）。
   *
   * 没有这个码时 `/notice/stream`、`/notice/unread-count`、`/notice/page` **全部 403**：
   * 建立连接 = 往控制台刷红。所以整条订阅（SSE + 轮询 + 拉最新）都先过这一道闸。
   * 判据只认 `userStore.hasPermission`（超管持 `*:*:*` 自然为 true），不另写第二套。
   */
  function canReadNotice(): boolean {
    return useUserStore().hasPermission(PERM.noticeMine);
  }

  /** 直接覆写未读数（拉接口成功后调用） */
  function setUnreadCount(count: number): void {
    unreadCount.value = Math.max(0, count);
    lastKnownUnread = unreadCount.value;
  }

  /**
   * 入队一条新消息（去重、新的在前、最多 5 条）。
   *
   * @returns 真的入队了（= 需要弹浮层）为 `true`；脏数据 / 重复消息为 `false`
   */
  function pushNotice(raw: unknown): boolean {
    const { items, accepted } = accumulator.push(raw);
    if (!accepted) {
      return false;
    }
    latestItems.value = [...items];
    newBatchCount.value += 1;
    popupSeq.value += 1;
    return true;
  }

  /**
   * 批量入队（拉「最新 5 条未读」时用）。
   *
   * 入参应当**新的在前**（后端分页就是）；这里倒序入队，让 `appendPopupItem` 的
   * `unshift` 把最终顺序还原成后端顺序。
   */
  function acceptBatch(rows: readonly unknown[]): number {
    let accepted = 0;
    for (const row of [...rows].reverse()) {
      if (pushNotice(row)) {
        accepted += 1;
      }
    }
    return accepted;
  }

  /** 清空本批（浮层关闭后调用）：只清展示用的列表与计数，不清「已见过」集合 */
  function resetPopupBatch(): void {
    latestItems.value = [...accumulator.clear()];
    newBatchCount.value = 0;
  }

  /**
   * 拉一次「最新 5 条未读」并入队。
   *
   * 用于：推送只带信号（没有消息体）、或轮询发现未读数上涨。
   *
   * @param sinceMs 只接受 `created_at >= sinceMs` 的条目（毫秒时间戳）。**必须给**：
   *   「最新 5 条未读」里往往混着几条早就存在、只是没点开的旧未读，全塞进浮层就会变成
   *   「新消息 5 条」却全是半小时前的旧消息（本卡第一轮真机验收实测过这个 bug）。
   */
  async function pullLatest(sinceMs?: number): Promise<void> {
    if (!canReadNotice()) {
      return;
    }
    try {
      const data = await pageMyNotices({ page: 1, page_size: MAX_POPUP_ITEMS, read_flag: false });
      const rows = sinceMs ? filterArrivedSince(data.list, sinceMs) : data.list;
      acceptBatch(rows);
    } catch {
      // 无 `business:notice:mine` 权限 / 网络失败：静默（浮层只是锦上添花）
    }
  }

  /**
   * 拉一次未读数。
   *
   * 顺带做「轮询兜底」：未读数**上涨**说明有新消息到达，而此时可能没有任何实时通道，
   * 于是补拉最新 5 条把浮层弹出来（首次拉取只建立基线，不弹）。
   */
  async function refreshUnread(): Promise<void> {
    if (!canReadNotice()) {
      // 无权限：不请求（必然 403），角标恒 0
      unreadCount.value = 0;
      return;
    }
    try {
      const res = await fetchUnreadCount();
      const next = Math.max(0, res.unread_count);
      const prev = lastKnownUnread;
      lastKnownUnread = next;
      unreadCount.value = next;
      const at = Date.now();
      if (prev !== null && next > prev) {
        // 只认「上一轮刷新之后」到达的消息（留 5 秒时钟/落库偏差）
        await pullLatest(at - 5_000);
      }
    } catch {
      // 未读数拉取失败（如无 business:notice:mine 权限）静默处理，不打扰用户
    }
  }

  /** 启动轮询兜底 */
  function startPolling(): void {
    stopPolling();
    pollTimer = window.setInterval(() => {
      void refreshUnread();
    }, POLL_INTERVAL_MS);
  }

  /** 停止轮询 */
  function stopPolling(): void {
    if (pollTimer) {
      window.clearInterval(pollTimer);
      pollTimer = undefined;
    }
  }

  /** 解析推送数据帧（失败返回 null，走「拉最新」兜底） */
  function parseFrameData(data: string): unknown {
    try {
      return JSON.parse(data);
    } catch {
      return null;
    }
  }

  /**
   * 处理一帧推送（SSE 自定义事件 / 带头流 / 无名 message 共用）。
   *
   * - 帧里带完整消息体（含 `receiver_id`）→ 直接入队，秒级弹出；
   * - 只有 `{type:'notice', count:1}` 这类信号 → 拉最新 5 条。
   */
  function handleFrame(frame: SseFrame): void {
    refreshToken.value += 1;
    const payload = parseFrameData(frame.data);
    const hasBody =
      payload !== null &&
      typeof payload === 'object' &&
      typeof (payload as Record<string, unknown>).receiver_id === 'string';
    if (hasBody) {
      const accepted = pushNotice(payload);
      if (accepted && lastKnownUnread !== null) {
        // 这条已经进浮层并计入角标了：把基线一起 +1，否则随后的 refreshUnread
        // 会把它当成「未读数上涨」，再触发一次「拉最新 5 条」，把几条旧未读也塞进浮层
        lastKnownUnread += 1;
      }
    } else {
      // 只推了信号（没有消息体）：拉最新 5 条，但只认最近 5 秒内落库的
      void pullLatest(Date.now() - 5_000);
    }
    void refreshUnread();
  }

  /** 停掉带头流并取消重连定时器 */
  function stopStream(): void {
    if (streamAbort) {
      streamAbort.abort();
      streamAbort = null;
    }
    if (streamRetryTimer) {
      window.clearTimeout(streamRetryTimer);
      streamRetryTimer = undefined;
    }
    streamRunning = false;
  }

  /** 一个轮询间隔后重试带头流（实时通道断了也不能永久放弃） */
  function scheduleStreamRetry(): void {
    if (streamRetryTimer) {
      return;
    }
    streamRetryTimer = window.setTimeout(() => {
      streamRetryTimer = undefined;
      void startStream();
    }, POLL_INTERVAL_MS);
  }

  /** 建立带头流；成功后停轮询，失败回到轮询 */
  async function startStream(): Promise<void> {
    if (streamRunning || streamAbort) {
      return;
    }
    const token = localStorage.getItem(TOKEN_KEY) || '';
    if (!token) {
      return;
    }
    streamRunning = true;
    const controller = new AbortController();
    streamAbort = controller;
    try {
      await openNoticeStream({
        url: buildNoticeStreamUrl(),
        token,
        signal: controller.signal,
        onOpen: () => {
          connected.value = true;
          stopPolling();
        },
        onFrame: handleFrame,
      });
      // 服务端正常关流：等一个间隔再重连
      connected.value = false;
      startPolling();
    } catch {
      connected.value = false;
      startPolling();
    } finally {
      streamRunning = false;
      if (streamAbort === controller) {
        streamAbort = null;
      }
      if (!connected.value) {
        scheduleStreamRetry();
      }
    }
  }

  /**
   * 建立实时订阅（带头流）。
   *
   * 新消息到达时：自增 `refreshToken`（通知页面刷新列表）、入队到 `latestItems`（弹浮层）并重拉未读数。
   * 断开 / 被拒（令牌失效、代理不支持长连接）时降级为「轮询」，一个轮询间隔后重试。
   *
   * # 为什么不再用 `EventSource`
   *
   * `EventSource` 不能设置 `Authorization` 头，只能走 `?token=`，而认证层只对白名单端点
   * 采信查询令牌 —— 漏配时它**每次页面加载都刷一条 401 并自动重连**（P1-FIX-6）。
   * 带头流与普通接口同一套认证，既不需要白名单，也不会产生这条噪声。
   */
  function connect(): void {
    if (!canReadNotice()) {
      // 无 `business:notice:mine`：SSE 握手与未读数都会 403 —— 一条连接都不建，
      // 角标恒 0、浮层无数据。先清掉可能残留的上一个账号的角标与浮层批次。
      disconnect();
      unreadCount.value = 0;
      resetPopupBatch();
      return;
    }
    if (streamRunning || streamAbort) {
      return;
    }
    void refreshUnread();
    // 先挂上轮询兜底：带头流握手成功（`onOpen`）时会被停掉
    startPolling();
    void startStream();
  }

  /** 断开订阅并清理定时器 */
  function disconnect(): void {
    stopStream();
    connected.value = false;
    stopPolling();
  }

  return {
    unreadCount,
    connected,
    refreshToken,
    latestItems,
    newBatchCount,
    popupSeq,
    setUnreadCount,
    refreshUnread,
    pushNotice,
    pullLatest,
    resetPopupBatch,
    connect,
    disconnect,
  };
});
