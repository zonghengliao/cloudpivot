/**
 * 菜单徽标状态（批次 E · E16）。
 *
 * # 三个职责
 *
 * 1. **数据**：`GET /api/system/badge/counts` 的四类计数，全局只有这一份
 *    （顶栏铃铛、消息中心页、侧边栏徽标都读同一份，避免各拉一次不同步）；
 * 2. **轮询**：30 秒一次；`document.visibilityState === 'hidden'` 时**停掉定时器**
 *    （不是「跳过请求」），回到前台立刻补一次；
 * 3. **映射**：把菜单路径映射到四个桶，并把目录的子菜单桶**去重**后相加。
 *
 * # 为什么目录要去重相加
 *
 * 工作任务的三视图（列表 / 看板 / 甘特 / 燃尽）是**同一批任务的不同展现**，
 * 所以徽标只挂在**目录** `/task` 上，子菜单不挂（否则四处显示同一个数字）。
 * 目录徽标的口径仍是「子树里出现过的**桶**去重后相加」。
 *
 * # 切路由为什么不重新请求
 *
 * `BasicLayout` 不会随路由重建，`startPolling()` 内部有 `polling` 闸（重复调用直接返回），
 * 数据留在 store 里，路由切换只读不拉。新数据由 30 秒轮询 / 回到前台时补拉。
 */
import { ref } from 'vue';
import { defineStore } from 'pinia';
import { EMPTY_BADGE_COUNTS, getBadgeCounts, type BadgeCounts } from '@/api/badge';
import type { MenuNode } from '@/api/system/auth-api';

/** 徽标桶名（= {@link BadgeCounts} 的键） */
export type BadgeKey = keyof BadgeCounts;

/** 默认轮询间隔（30 秒：够及时，又不给后端添压力） */
export const BADGE_POLL_INTERVAL_MS = 30_000;

/**
 * 菜单路径 → 徽标桶。
 *
 * 路径取自 `sys_menu.path`（实测值，见 `E16` 验证证据的菜单表），**逐字匹配**：
 * 改菜单路径就必须同步改这里，否则那个菜单的徽标静默变 0。
 */
const PATH_BADGE: Record<string, BadgeKey> = {
  '/announcement': 'announcement_unread',
  '/business/notice/message': 'notice_unread',
  '/flow/task': 'flow_todo',
  // 工作任务：只在**目录**上挂徽标（三视图是同一批任务的不同展现，
  // 每个子菜单各显示一个同样的数字只是视觉噪音）。
  // `badgeKeysOfSubtree` 会收集目录自身的 path，所以目录仍有徽标。
  '/task': 'task_todo',
};

/** 去掉尾部斜杠（`/task/list/` 与 `/task/list` 视为同一菜单） */
function normalizePath(path: string): string {
  const trimmed = path.replace(/\/+$/, '');
  return trimmed || '/';
}

/**
 * 菜单路径 → 徽标桶（不显示徽标的菜单返回 `null`）。
 *
 * 明确返回 `null` 而不是 0 的情况：公告管理、消息通知、流程定义、发起审批、
 * 项目、帮助中心 —— 这些菜单「不是待我处理的东西」，值恒为 0 也不显示红点。
 */
export function badgeKeyOfPath(path?: string | null): BadgeKey | null {
  if (!path) {
    return null;
  }
  return PATH_BADGE[normalizePath(path)] ?? null;
}

/**
 * 收集一棵菜单子树里出现过的徽标桶（去重）。
 *
 * 递归到底：目录套目录时（如「业务 → 消息 → 消息中心」）也能把桶带上来。
 */
export function badgeKeysOfSubtree(menu: MenuNode, keys: Set<BadgeKey> = new Set()): BadgeKey[] {
  const own = badgeKeyOfPath(menu.path);
  if (own) {
    keys.add(own);
  }
  for (const child of menu.children ?? []) {
    badgeKeysOfSubtree(child, keys);
  }
  return [...keys];
}

export const useBadgeStore = defineStore('badge', () => {
  /** 四类计数（初值全 0 → 不显示任何红点） */
  const counts = ref<BadgeCounts>({ ...EMPTY_BADGE_COUNTS });
  /** 是否至少成功拉过一次（排障用，不参与渲染） */
  const loaded = ref(false);

  /** 定时器句柄；`undefined` = 未排程 */
  let timer: number | undefined;
  /** 是否处于轮询状态（`startPolling` 的重复调用闸） */
  let polling = false;
  /** 当前轮询间隔 */
  let intervalMs = BADGE_POLL_INTERVAL_MS;

  /** 排一个定时器（已排过就复用，不叠加） */
  function schedule(): void {
    if (timer !== undefined) {
      return;
    }
    timer = window.setInterval(() => {
      void refresh();
    }, intervalMs);
  }

  /** 撤掉定时器 */
  function unschedule(): void {
    if (timer !== undefined) {
      window.clearInterval(timer);
      timer = undefined;
    }
  }

  /** 页面可见性变化：隐藏就停表，回到前台立即补拉 + 重新排表 */
  function onVisibilityChange(): void {
    if (!polling) {
      return;
    }
    if (document.visibilityState === 'hidden') {
      unschedule();
      return;
    }
    void refresh();
    schedule();
  }

  /**
   * 拉一次徽标计数。
   *
   * 失败**静默**（`catch` 不弹错）：徽标是辅助信息，网络抖动时不该打断用户操作；
   * 保留上一次的成功值，比清成 0 更接近真实（清 0 会让红点闪一下消失）。
   */
  async function refresh(): Promise<void> {
    try {
      counts.value = await getBadgeCounts();
      loaded.value = true;
    } catch {
      // 401 由 request 拦截器统一处理（跳登录）；其余情况保留旧值
    }
  }

  /** 某个菜单路径的徽标数（不显示徽标的菜单恒为 0） */
  function countOfPath(path?: string | null): number {
    const key = badgeKeyOfPath(path);
    return key ? counts.value[key] : 0;
  }

  /**
   * 菜单节点的徽标数。
   *
   * - 叶子菜单 → 自己的桶；
   * - 目录 → 子树里出现过的**桶去重**后相加（任务三视图共用 `task_todo`，只算一次）。
   */
  function badgeOfMenu(menu: MenuNode): number {
    const children = menu.children ?? [];
    if (children.length === 0) {
      return countOfPath(menu.path);
    }
    let sum = 0;
    for (const key of badgeKeysOfSubtree(menu)) {
      sum += counts.value[key];
    }
    return sum;
  }

  /**
   * 启动轮询（`BasicLayout` 挂载时调用）。
   *
   * 重复调用安全：已经在轮询就直接返回，不会叠加定时器、也不会多打一次接口。
   */
  function startPolling(ms: number = BADGE_POLL_INTERVAL_MS): void {
    if (polling) {
      return;
    }
    polling = true;
    intervalMs = ms > 0 ? ms : BADGE_POLL_INTERVAL_MS;
    // 挂载即拉一次：不能等 30 秒才出红点
    void refresh();
    if (document.visibilityState !== 'hidden') {
      schedule();
    }
    document.addEventListener('visibilitychange', onVisibilityChange);
  }

  /** 停止轮询（`BasicLayout` 卸载时调用） */
  function stopPolling(): void {
    polling = false;
    unschedule();
    document.removeEventListener('visibilitychange', onVisibilityChange);
  }

  return {
    counts,
    loaded,
    refresh,
    countOfPath,
    badgeOfMenu,
    startPolling,
    stopPolling,
  };
});
