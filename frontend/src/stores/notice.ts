import { ref } from 'vue';
import { defineStore } from 'pinia';
import { buildNoticeSseUrl, fetchUnreadCount } from '@/api/business/notice';

/** 未读数轮询间隔（SSE 不可用时的兜底；30 秒足够，不给后端添压力） */
const POLL_INTERVAL_MS = 30_000;

/**
 * 消息通知状态：未读数、SSE 连接与「有新消息」信号。
 *
 * # 生命周期
 *
 * 由 `BasicLayout` 在挂载时 `connect()`、卸载时 `disconnect()`，全局只有一条连接；
 * 消息中心页面复用同一份未读数，避免顶栏与页面各拉一次导致角标不同步。
 */
export const useNoticeStore = defineStore('notice', () => {
  /** 未读条数（顶栏角标与消息中心共用） */
  const unreadCount = ref(0);
  /** 是否已建立 SSE 连接 */
  const connected = ref(false);
  /** 收到推送时自增，供页面 watch 后刷新列表 */
  const refreshToken = ref(0);

  let source: EventSource | null = null;
  let pollTimer: number | undefined;

  /** 直接覆写未读数（拉接口成功后调用） */
  function setUnreadCount(count: number): void {
    unreadCount.value = Math.max(0, count);
  }

  /** 拉一次未读数 */
  async function refreshUnread(): Promise<void> {
    try {
      const res = await fetchUnreadCount();
      setUnreadCount(res.unread_count);
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

  /**
   * 建立 SSE 订阅。
   *
   * 新消息到达时：自增 `refreshToken`（通知页面刷新列表）并重拉未读数。
   * `EventSource` 出错（令牌失效 / 后端未开 SSE / 代理不支持）时降级为轮询。
   */
  function connect(): void {
    if (source) {
      return;
    }
    void refreshUnread();
    try {
      source = new EventSource(buildNoticeSseUrl());
      source.onopen = () => {
        connected.value = true;
        stopPolling();
      };
      source.onmessage = () => {
        refreshToken.value += 1;
        void refreshUnread();
      };
      source.onerror = () => {
        connected.value = false;
        // EventSource 会自动重连，这里只补一条轮询兜底，避免长时间无更新
        startPolling();
      };
    } catch {
      connected.value = false;
      startPolling();
    }
  }

  /** 断开订阅并清理定时器 */
  function disconnect(): void {
    if (source) {
      source.close();
      source = null;
    }
    connected.value = false;
    stopPolling();
  }

  return {
    unreadCount,
    connected,
    refreshToken,
    setUnreadCount,
    refreshUnread,
    connect,
    disconnect,
  };
});
