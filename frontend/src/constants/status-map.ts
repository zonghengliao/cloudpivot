/**
 * 状态徽标映射（**唯一来源**）。
 *
 * 三套域共用一个徽标组件 `<StatusBadge domain="task" :status="row.status" />`：
 * 文案 / 主题 / 圆点色只在本文件定义，页面里**不许**再出现
 * `status === 'DOING' ? '进行中' : ...` 这类内联映射 —— 否则同一个状态在列表页、
 * 详情页、看板页会各长一副样子（公告模块已经踩过一次）。
 *
 * # 本期范围（T6a）
 *
 * 只落地 `task` 域。`announcement` / `import` 先留空表，后续小卡把各自页面里的
 * `statusLabel()` / `statusTheme()` 抽过来即可 —— 组件与调用方都不用改。
 *
 * # 颜色口径
 *
 * `dotVar` 一律是**主题变量**（`--td-*`），不写死 hex：
 * 配色方案切换（6 套 × 明暗）与暗色模式下，徽标必须跟着变。
 */

/** 徽标域：本期只实现 task，另两套为后续扩展位 */
export type StatusDomain = 'task' | 'announcement' | 'import';

/** TDesign `t-tag` 的主题取值 */
export type StatusTagTheme = 'default' | 'primary' | 'success' | 'warning' | 'danger';

/** 单个状态的展示元信息 */
export interface StatusMeta {
  /** 中文文案 */
  label: string;
  /** t-tag 主题（颜色由 TDesign 主题变量驱动） */
  theme: StatusTagTheme;
  /** 圆点颜色，必须是主题变量 */
  dotVar: string;
}

/** 任务状态（与后端 `business/task/dto.rs` 的 `STATUSES` 逐字一致，大小写不许改） */
export const TASK_STATUSES = ['TODO', 'DOING', 'REVIEW', 'DONE', 'CANCELLED'] as const;

/** 任务状态字面量类型 */
export type TaskStatus = (typeof TASK_STATUSES)[number];

/** 任务优先级（后端 `PRIORITIES`：P0 最高 → P3 最低） */
export const TASK_PRIORITIES = ['P0', 'P1', 'P2', 'P3'] as const;

/** 任务优先级字面量类型 */
export type TaskPriority = (typeof TASK_PRIORITIES)[number];

/** 优先级展示元信息 */
export interface PriorityMeta {
  /** 徽标主文案（`P0`–`P3`） */
  label: string;
  /** 中文语义（悬浮提示用） */
  name: string;
  /** t-tag 主题 */
  theme: StatusTagTheme;
  /** 圆点颜色（主题变量） */
  dotVar: string;
}

/** 优先级映射：P0 最高（danger）→ P3 最低（default） */
export const PRIORITY_MAP: Record<TaskPriority, PriorityMeta> = {
  P0: { label: 'P0', name: '最高', theme: 'danger', dotVar: 'var(--td-error-color)' },
  P1: { label: 'P1', name: '高', theme: 'warning', dotVar: 'var(--td-warning-color)' },
  P2: { label: 'P2', name: '中', theme: 'primary', dotVar: 'var(--td-brand-color)' },
  P3: { label: 'P3', name: '低', theme: 'default', dotVar: 'var(--td-text-color-placeholder)' },
};

/** 任务状态映射（顺序 = 状态机推进顺序，筛选下拉按此排列） */
const TASK_STATUS_MAP: Record<string, StatusMeta> = {
  TODO: { label: '待开始', theme: 'default', dotVar: 'var(--td-text-color-placeholder)' },
  DOING: { label: '进行中', theme: 'primary', dotVar: 'var(--td-brand-color)' },
  REVIEW: { label: '待验收', theme: 'warning', dotVar: 'var(--td-warning-color)' },
  DONE: { label: '已完成', theme: 'success', dotVar: 'var(--td-success-color)' },
  CANCELLED: { label: '已取消', theme: 'default', dotVar: 'var(--td-text-color-disabled)' },
};

/**
 * 全部域的映射表。
 *
 * `announcement` / `import` 为**扩展位**：留空对象，等对应页面迁移时填充。
 * 空表不会出错 —— {@link statusMeta} 的兜底会把未知状态原样显示为灰色徽标。
 */
export const STATUS_MAP: Record<StatusDomain, Record<string, StatusMeta>> = {
  task: TASK_STATUS_MAP,
  // TODO(批次C+·T6 后续小卡)：把公告管理的 DRAFT/PUBLISHED/OFFLINE/EXPIRED 抽到这里
  announcement: {},
  // TODO(批次C+·T6 后续小卡)：把导入/导出任务的状态抽到这里
  import: {},
};

/** 未知状态的兜底元信息（原样显示状态字面量，灰色） */
const FALLBACK_META: StatusMeta = {
  label: '—',
  theme: 'default',
  dotVar: 'var(--td-text-color-placeholder)',
};

/**
 * 取某域某状态的展示元信息。
 *
 * # 参数
 * - `domain`：徽标域
 * - `status`：后端下发的状态字面量（大小写不敏感，内部统一转大写）
 *
 * # 返回值
 * 命中映射返回对应元信息；未命中返回兜底（`label` 为原字面量，灰色）——
 * 后端新增状态时页面**不白屏**，只会看到一个没上色的原始值，便于暴露漏配。
 */
export function statusMeta(domain: StatusDomain, status: string): StatusMeta {
  const key = (status ?? '').toString().trim().toUpperCase();
  const table = STATUS_MAP[domain] ?? {};
  return table[key] ?? { ...FALLBACK_META, label: key || FALLBACK_META.label };
}

/** 取某域某状态的中文文案 */
export function statusLabel(domain: StatusDomain, status: string): string {
  return statusMeta(domain, status).label;
}

/**
 * 生成筛选下拉选项（按映射表顺序）。
 *
 * # 参数
 * - `domain`：徽标域
 *
 * # 返回值
 * `{ label, value, theme }` 数组；空域返回空数组。
 */
export function statusOptions(domain: StatusDomain): Array<{ label: string; value: string; theme: StatusTagTheme }> {
  return Object.entries(STATUS_MAP[domain] ?? {}).map(([value, meta]) => ({
    label: meta.label,
    value,
    theme: meta.theme,
  }));
}

/** 优先级筛选/表单选项（顺序 P0 → P3） */
export function priorityOptions(): Array<{ label: string; value: TaskPriority }> {
  return TASK_PRIORITIES.map((value) => ({ label: `${value} ${PRIORITY_MAP[value].name}`, value }));
}

/**
 * 取优先级元信息（未知值兜底为灰色原值）。
 *
 * # 参数
 * - `priority`：后端下发的优先级字面量
 */
export function priorityMeta(priority: string): PriorityMeta {
  const key = (priority ?? '').toString().trim().toUpperCase();
  const hit = PRIORITY_MAP[key as TaskPriority];
  return hit ?? { label: key || '—', name: '未定义', theme: 'default', dotVar: 'var(--td-text-color-placeholder)' };
}
