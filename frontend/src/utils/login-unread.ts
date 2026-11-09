/**
 * 登录后「你有 N 条待处理」汇总弹窗的**纯逻辑**：4 类未读数的采集、汇总与「本次登录只弹一次」标记。
 *
 * 与 `notice-popup.ts` 同样的理由抽成无副作用模块：单测跑在 Node（tsx）下，
 * import 任何 `@/api/*` 都会因顶层 `import.meta.env` 抛错，所以**接口调用以 loader 形式注入**。
 *
 * # 数据来源（2026-11-03 逐个实测）
 *
 * | 类别 | 接口 | 取值 |
 * |------|------|------|
 * | 站内信 | `GET /api/business/notice/unread-count` | `data.unread_count` |
 * | 公告 | `GET /api/business/announcement/mine/unread-count` | `data.announcement_unread` |
 * | 工作任务 | `GET /api/business/task/page?status=TODO&owner_id=<me>&page_size=1` | `data.total` |
 * | 审批待办 | `GET /api/business/flow/task/todo?page=1&page_size=1` | `data.total` |
 *
 * ⚠ 卡面写的 `GET /business/flow/task/page?status=PENDING` **不存在**（实测 `404 请求的资源不存在`），
 * 待办走的是 `/flow/task/todo`（后端就是「只返回 PENDING 的 APPROVAL + RESUBMIT」）。
 * 工作任务也没有独立 count 接口，用 `page_size=1` 取 `total`。
 *
 * # 权限（2026-11-09 HOME-6）
 *
 * 四类接口各有自己的 `#[nik::permission]` 权限码，见 {@link UNREAD_SOURCES} 的 `permission` 字段。
 * **没这个权限码的账号，那一路请求必然 403**（例：种子 `EMPLOYEE` 角色没有
 * `business:notice:mine` / `business:announcement:read`，登录一次控制台红两条）。
 * 所以登录弹窗只对「有权限」的类别发请求，也只列出有权限的行 —— 判据见
 * {@link permittedUnreadKeys}，别在组件里另写一套。
 */
import { PERM } from '@/constants/permissions';

/** sessionStorage 标记键：本次登录已经弹过（刷新不重复弹） */
export const LOGIN_UNREAD_SHOWN_KEY = 'login-unread-shown';

/**
 * 登录汇总弹窗的四个类别（顺序即界面行顺序）。
 *
 * `permission` 是取该类未读数所需的后端权限码，**逐字**对应 `#[nik::permission]`：
 * - 站内信 `/notice/unread-count` → `business:notice:mine`
 * - 公告 `/announcement/unread-count` → `business:announcement:read`
 * - 工作任务 `/task/page` → `business:task:read`
 * - 审批待办 `/flow/task/todo` → `business:flow:instance:read`
 */
export const UNREAD_SOURCES = [
  {
    key: 'notice',
    label: '站内信',
    icon: 'mail',
    path: '/business/notice/message',
    permission: PERM.noticeMine,
  },
  {
    key: 'announcement',
    label: '公告',
    icon: 'notification',
    path: '/announcement',
    permission: PERM.announcementRead,
  },
  {
    key: 'task',
    label: '工作任务',
    icon: 'task',
    path: '/task/list',
    permission: PERM.taskRead,
  },
  {
    key: 'approval',
    label: '审批待办',
    icon: 'check-circle',
    path: '/flow/task',
    permission: PERM.flowInstanceRead,
  },
] as const;

/** 类别键：`notice` / `announcement` / `task` / `approval` */
export type UnreadKey = (typeof UNREAD_SOURCES)[number]['key'];

/** 四类未读数 */
export type UnreadCounts = Record<UnreadKey, number>;

/** 单类取数函数（组件里接真接口，单测里接假数据） */
export type UnreadLoader = () => Promise<number>;

/** 取数函数表（缺项按 0 处理，便于分阶段接接口） */
export type UnreadLoaders = Partial<Record<UnreadKey, UnreadLoader>>;

/** 弹窗里的一行 */
export interface UnreadRow {
  /** 类别键 */
  key: UnreadKey;
  /** 类别名（站内信 / 公告 / 工作任务 / 审批待办） */
  label: string;
  /** TDesign 图标名 */
  icon: string;
  /** 「查看」跳转路径 */
  path: string;
  /** 未读条数 */
  count: number;
}

/** 全 0 的计数表 */
export function emptyCounts(): UnreadCounts {
  return { notice: 0, announcement: 0, task: 0, approval: 0 };
}

/** 把外部数值规整成非负整数（后端返回 `null` / 字符串 / 负数都不该让界面崩） */
function toCount(value: unknown): number {
  const num = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(num) || num <= 0) {
    return 0;
  }
  return Math.floor(num);
}

/**
 * 当前账号**有权限**取数的类别（无权限的类别按「0 条」处理）。
 *
 * 这是「未读弹窗不发注定 403 的请求」的唯一判据：不在返回结果里的类别
 * **既不请求，也不出现在弹窗行里**（无权限 = 看不到那一类，行留着只会点进去再撞 403）。
 *
 * @param has 权限判定函数，组件里传 `userStore.hasPermission`
 * @returns 保持 {@link UNREAD_SOURCES} 的原顺序
 */
export function permittedUnreadKeys(has: (code: string) => boolean): UnreadKey[] {
  return UNREAD_SOURCES.filter((source) => has(source.permission)).map((source) => source.key);
}

/**
 * 只保留有权限类别的取数器（其余整条剔除，`collectUnread` 就不会去调它们）。
 */
export function pickUnreadLoaders(
  loaders: UnreadLoaders,
  keys: readonly UnreadKey[],
): UnreadLoaders {
  const picked: UnreadLoaders = {};
  for (const key of keys) {
    const loader = loaders[key];
    if (loader) {
      picked[key] = loader;
    }
  }
  return picked;
}

/**
 * 并行取四类未读数。
 *
 * 单类失败（无权限 403 / 接口 404）按 **0** 计：登录弹窗是**提示**，不能因为其中一个接口
 * 抽风就整个弹窗不弹，也不能把别人的失败抛到用户面前（`Promise.all` 会整体 reject）。
 */
export async function collectUnread(loaders: UnreadLoaders): Promise<UnreadCounts> {
  const keys = UNREAD_SOURCES.map((source) => source.key);
  const results = await Promise.allSettled(
    keys.map(async (key) => {
      const loader = loaders[key];
      if (!loader) {
        return 0;
      }
      return toCount(await loader());
    }),
  );
  const counts = emptyCounts();
  keys.forEach((key, index) => {
    const settled = results[index];
    counts[key] = settled.status === 'fulfilled' ? toCount(settled.value) : 0;
  });
  return counts;
}

/** 四类合计 */
export function totalUnread(counts: UnreadCounts): number {
  return UNREAD_SOURCES.reduce((sum, source) => sum + toCount(counts[source.key]), 0);
}

/**
 * 汇总成弹窗行。
 *
 * 默认 4 行恒定返回（0 条的行由界面置灰，便于用户知道「这一类我一条没落」）；
 * 传 `keys` 时只返回这些类别 —— 登录弹窗用「有权限的类别」作参数，
 * 让无权限的行根本不出现。
 */
export function summarizeUnread(counts: UnreadCounts, keys?: readonly UnreadKey[]): UnreadRow[] {
  const sources = keys
    ? UNREAD_SOURCES.filter((source) => keys.includes(source.key))
    : UNREAD_SOURCES;
  return sources.map((source) => ({
    key: source.key,
    label: source.label,
    icon: source.icon,
    path: source.path,
    count: toCount(counts[source.key]),
  }));
}

// ===== 「本次登录只弹一次」标记 =====

/** 无 sessionStorage 时的进程内兜底（隐私模式 / Node 单测 / 嵌入式 WebView） */
let memoryShown = false;

/** 安全拿 sessionStorage：某些隐私模式下**访问属性本身**就会抛 SecurityError */
function safeSessionStorage(): Storage | null {
  try {
    if (typeof window === 'undefined' || !window.sessionStorage) {
      return null;
    }
    return window.sessionStorage;
  } catch {
    return null;
  }
}

/** 本次登录是否已经弹过 */
export function isLoginUnreadShown(): boolean {
  const storage = safeSessionStorage();
  if (!storage) {
    return memoryShown;
  }
  try {
    return storage.getItem(LOGIN_UNREAD_SHOWN_KEY) === '1';
  } catch {
    return memoryShown;
  }
}

/** 标记本次登录已弹过（弹窗打开或关闭时调用） */
export function markLoginUnreadShown(): void {
  memoryShown = true;
  const storage = safeSessionStorage();
  if (!storage) {
    return;
  }
  try {
    storage.setItem(LOGIN_UNREAD_SHOWN_KEY, '1');
  } catch {
    // 存储被禁用：memoryShown 已生效，本次会话内不会重复弹
  }
}

/** 清标记（退出登录时调用，下次登录重新弹） */
export function clearLoginUnreadShown(): void {
  memoryShown = false;
  const storage = safeSessionStorage();
  if (!storage) {
    return;
  }
  try {
    storage.removeItem(LOGIN_UNREAD_SHOWN_KEY);
  } catch {
    // 同上：清不掉也不影响 memoryShown
  }
}
