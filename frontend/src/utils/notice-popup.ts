/**
 * 新通知浮层（右上角自动弹出）的**纯逻辑**：不碰 DOM、不碰接口，便于单测。
 *
 * # 为什么单独抽一层
 *
 * 本仓单测跑在 `tsx --test` 下（Node，无 jsdom），而 `@/api/request` 顶层就用了
 * `import.meta.env`，**在 Node 里 import 任何 api 模块都会抛 TypeError** —— 于是
 * `stores/notice.ts`（会 import api）整条链路在单测里不可加载。把「累积 / 上限 / 去重 /
 * 计时」这些真正要回归的规则放在这里，`stores/notice.ts` 与 `NoticeToast.vue` 都只做接线。
 *
 * # 与后端的契约（2026-11-03 实测）
 *
 * SSE 数据帧：`event: notice` + `data: {notice_id, notice_no, notice_type, level, title,
 * link_url, biz_type, biz_id, receiver_id, created_at}` —— 是**完整消息体**（没有 content），
 * 因此可以直接入队；若只收到「有新消息」的信号（例如轮询发现未读数上涨），调用方改走
 * `pageMyNotices({page_size:5, read_flag:false})` 拉最新 5 条。
 */

/** 浮层最多展示的条目数（与铃铛面板一致：最近 5 条） */
export const MAX_POPUP_ITEMS = 5;

/** 自动收起时长（毫秒）：8 秒 */
export const POPUP_AUTO_CLOSE_MS = 8_000;

/** 浮层里的一条消息（SSE 帧 / `MyNoticeRes` 的子集，两种来源都能直接落进来） */
export interface NoticePopupItem {
  /** 该用户自己的接收行 id（跳转 `?focus=` 与去重都用它） */
  receiver_id: string;
  /** 通知主键 */
  notice_id?: string;
  /** 类型：`SYSTEM` / `TODO` / `ALERT` / `WORKFLOW` / `CIRCULAR` */
  notice_type: string;
  /** 级别：`INFO` / `SUCCESS` / `WARN` / `ERROR` */
  level: string;
  /** 标题（浮层只显示标题，正文留给消息中心） */
  title: string;
  /** 接收时间（RFC 3339，可能是纳秒精度，dayjs 能解析） */
  created_at: string;
  /** 点击跳转地址（可空；浮层统一跳消息中心带 focus，这里只做兜底保留） */
  link_url?: string | null;
}

/** 通知类型 → 图标 / 中文名（TDesign 图标名，均已确认存在） */
const TYPE_META: Record<string, { icon: string; label: string }> = {
  SYSTEM: { icon: 'mail', label: '系统' },
  TODO: { icon: 'task', label: '待办' },
  ALERT: { icon: 'error-circle', label: '预警' },
  WORKFLOW: { icon: 'check-circle', label: '审批' },
  CIRCULAR: { icon: 'notification', label: '公告' },
};

/** 未知类型回落（后端将来加类型时不至于空白图标） */
const TYPE_FALLBACK = { icon: 'notification', label: '通知' };

/** 取类型对应的图标与中文名 */
export function noticeTypeMeta(type?: string | null): { icon: string; label: string } {
  const key = (type ?? '').toUpperCase();
  return TYPE_META[key] ?? TYPE_FALLBACK;
}

/** 级别 → 语义色类名后缀（`info` / `success` / `warn` / `error`） */
export function levelKey(level?: string | null): 'info' | 'success' | 'warn' | 'error' {
  switch ((level ?? '').toUpperCase()) {
    case 'SUCCESS':
      return 'success';
    case 'WARN':
    case 'URGENT':
      return 'warn';
    case 'ERROR':
      return 'error';
    default:
      return 'info';
  }
}

/**
 * 把一条原始数据（SSE 帧解析结果 / 接口行）规整成浮层条目。
 *
 * 缺 `receiver_id` 或 `title` 的脏数据返回 `null`，调用方直接丢弃 —— 浮层宁可少一条，
 * 也不能渲染出一个没有主键（点了不知道跳哪）的空行。
 */
export function normalizePopupItem(raw: unknown): NoticePopupItem | null {
  if (!raw || typeof raw !== 'object') {
    return null;
  }
  const record = raw as Record<string, unknown>;
  const receiverId = typeof record.receiver_id === 'string' ? record.receiver_id.trim() : '';
  const title = typeof record.title === 'string' ? record.title.trim() : '';
  if (!receiverId || !title) {
    return null;
  }
  const createdAt = typeof record.created_at === 'string' && record.created_at ? record.created_at : new Date().toISOString();
  return {
    receiver_id: receiverId,
    notice_id: typeof record.notice_id === 'string' ? record.notice_id : undefined,
    notice_type: typeof record.notice_type === 'string' ? record.notice_type : 'SYSTEM',
    level: typeof record.level === 'string' ? record.level : 'INFO',
    title,
    created_at: createdAt,
    link_url: typeof record.link_url === 'string' ? record.link_url : null,
  };
}

/**
 * 把新条目并入「最新消息」列表：**新的在前、按 `receiver_id` 去重、最多 `max` 条**。
 *
 * 去重是必需的：一条通知既可能从 SSE 帧直接到，也可能被随后的「拉最新 5 条」再补一次
 * （两条通道都要留，见 store 注释），不去重就会看到重复行、标题条数也虚高。
 */
export function appendPopupItem(
  list: readonly NoticePopupItem[],
  item: NoticePopupItem,
  max: number = MAX_POPUP_ITEMS,
): NoticePopupItem[] {
  const rest = list.filter((existing) => existing.receiver_id !== item.receiver_id);
  return [item, ...rest].slice(0, Math.max(1, max));
}

/**
 * 相对时间文案：刚刚 / N 分钟前 / N 小时前 / N 天前 / `MM-DD HH:mm`。
 *
 * 浮层空间比铃铛面板更紧，绝对时间（`2026-11-03 22:49:36`）会把标题挤成两行。
 * 未来时间（时钟漂移）按「刚刚」处理，不显示「-1 分钟前」。
 */
export function formatRelativeTime(value?: string | null, now: number = Date.now()): string {
  if (!value) {
    return '-';
  }
  const parsed = new Date(value);
  const time = parsed.getTime();
  if (Number.isNaN(time)) {
    return '-';
  }
  const diff = now - time;
  if (diff < 60_000) {
    return '刚刚';
  }
  if (diff < 3_600_000) {
    return `${Math.floor(diff / 60_000)} 分钟前`;
  }
  if (diff < 86_400_000) {
    return `${Math.floor(diff / 3_600_000)} 小时前`;
  }
  if (diff < 7 * 86_400_000) {
    return `${Math.floor(diff / 86_400_000)} 天前`;
  }
  const pad = (n: number): string => String(n).padStart(2, '0');
  return `${pad(parsed.getMonth() + 1)}-${pad(parsed.getDate())} ${pad(parsed.getHours())}:${pad(parsed.getMinutes())}`;
}

/**
 * 「最新消息」累加器：**去重 + 新的在前 + 上限 5 条 + 跨批次记忆已见过**。
 *
 * `stores/notice.ts` 的 `latestItems` 就是它的输出 —— 把规则放在这里是因为 store 要 import
 * `@/api/*`（Node 单测里加载不了），而这些规则恰恰是唯一需要回归的部分。
 *
 * `clear()` 只清「当前这一批」（浮层关闭后），不清 `seen`：同一条消息不会因为浮层关掉再弹一次，
 * 也不会被 SSE / 带头流 / 「拉最新 5 条」三条通道重复计入条数。
 */
export class NoticeAccumulator {
  /** 当前批次的条目（新的在前） */
  private items: NoticePopupItem[] = [];

  /** 已入队过的接收行 id（跨批次） */
  private readonly seen = new Set<string>();

  /** 单批上限 */
  private readonly max: number;

  /** `seen` 上限：只为防内存无限增长，超出按插入顺序淘汰最早的 */
  private readonly seenLimit: number;

  constructor(max: number = MAX_POPUP_ITEMS, seenLimit: number = 500) {
    this.max = Math.max(1, max);
    this.seenLimit = Math.max(this.max, seenLimit);
  }

  /** 当前列表（新的在前） */
  get current(): readonly NoticePopupItem[] {
    return this.items;
  }

  /** 当前批次的条数 */
  get count(): number {
    return this.items.length;
  }

  /** 是否已经见过这条接收行 */
  has(receiverId: string): boolean {
    return this.seen.has(receiverId);
  }

  /**
   * 入队一条。
   *
   * @returns `accepted=false` 表示脏数据或重复消息（调用方据此决定「不弹浮层」）
   */
  push(raw: unknown): { items: NoticePopupItem[]; accepted: boolean } {
    const item = normalizePopupItem(raw);
    if (!item || this.seen.has(item.receiver_id)) {
      return { items: this.items, accepted: false };
    }
    this.seen.add(item.receiver_id);
    if (this.seen.size > this.seenLimit) {
      const keys = Array.from(this.seen);
      for (const key of keys.slice(0, keys.length - this.seenLimit)) {
        this.seen.delete(key);
      }
    }
    this.items = appendPopupItem(this.items, item, this.max);
    return { items: this.items, accepted: true };
  }

  /** 清空当前批次（浮层关闭时调），保留 `seen` */
  clear(): NoticePopupItem[] {
    this.items = [];
    return this.items;
  }
}

/**
 * 过滤出「`sinceMs` 之后到达」的条目（浮层只弹真正的新消息）。
 *
 * 为什么要过滤：「拉最新 5 条未读」里常常混着几条早就存在、只是没点开的旧未读，
 * 直接入队就会变成「新消息 5 条」却全是半小时前的旧消息（本卡第一轮真机验收实测到过）。
 * `created_at` 解析失败的脏数据**保留**（宁可多显示一条，也不要因为格式变化把新消息吞掉）。
 */
export function filterArrivedSince<T extends { created_at?: string | null }>(
  rows: readonly T[],
  sinceMs: number,
): T[] {
  return rows.filter((row) => {
    const at = row.created_at ? Date.parse(row.created_at) : Number.NaN;
    return Number.isNaN(at) || at >= sinceMs;
  });
}

/**
 * 自动收起计时器：支持「悬停暂停 / 离开续跑 / 新消息到达重新计时」。
 *
 * 暂停用「剩余毫秒」记，而不是「暂停起点」—— 连续悬停/离开多次也不会累积误差。
 * 到期回调只触发一次，触发后 `active` 变 `false`，调用方据此关浮层。
 */
export class AutoCloseTimer {
  /** 当前是否在计时（暂停期间也是 true，表示「这条倒计时还没走完」） */
  private handle: ReturnType<typeof setTimeout> | null = null;

  /** 暂停中 */
  private paused = false;

  /** 本次计时的到期时间戳（暂停时用来算剩余量） */
  private deadline = 0;

  /** 暂停时的剩余毫秒 */
  private remaining = 0;

  /** 完整时长 */
  private readonly duration: number;

  /** 到期回调 */
  private readonly onExpire: () => void;

  constructor(onExpire: () => void, duration: number = POPUP_AUTO_CLOSE_MS) {
    this.onExpire = onExpire;
    this.duration = Math.max(1, duration);
  }

  /** 计时中（含暂停）为 true；已到期或已取消为 false */
  get active(): boolean {
    return this.handle !== null;
  }

  /** 暂停中为 true */
  get isPaused(): boolean {
    return this.paused;
  }

  /** 从头开始计时（新消息到达 / 浮层刚出现时调用） */
  start(): void {
    this.clearHandle();
    this.paused = false;
    this.remaining = this.duration;
    this.schedule(this.duration);
  }

  /** 悬停：暂停倒计时，保留剩余量 */
  pause(): void {
    if (this.paused || this.handle === null) {
      return;
    }
    this.remaining = Math.max(0, this.deadline - Date.now());
    this.clearHandle();
    this.paused = true;
  }

  /** 离开：接着暂停前的剩余量继续 */
  resume(): void {
    if (!this.paused) {
      return;
    }
    this.paused = false;
    this.schedule(Math.max(1, this.remaining));
  }

  /** 取消（用户手动关闭 / 组件卸载） */
  cancel(): void {
    this.clearHandle();
    this.paused = false;
    this.remaining = 0;
  }

  /** 剩余毫秒（测试与调试用；未计时为 0） */
  get remainingMs(): number {
    if (this.paused) {
      return this.remaining;
    }
    return this.handle === null ? 0 : Math.max(0, this.deadline - Date.now());
  }

  private schedule(ms: number): void {
    this.deadline = Date.now() + ms;
    this.handle = setTimeout(() => {
      this.handle = null;
      this.paused = false;
      this.remaining = 0;
      this.onExpire();
    }, ms);
  }

  private clearHandle(): void {
    if (this.handle !== null) {
      clearTimeout(this.handle);
      this.handle = null;
    }
  }
}
