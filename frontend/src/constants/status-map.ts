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

/** 徽标域：task / announcement / import / 审批流（流程定义 + 审批实例 + 节点 + 任务 + 抄送） */
export type StatusDomain =
  | 'task'
  | 'announcement'
  | 'import'
  | 'erp-contract'
  | 'flow-def'
  | 'flow-instance'
  | 'flow-node'
  | 'flow-task'
  | 'flow-cc';

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
 * 审批流状态映射（批次 D · D5）。
 *
 * 三套域：
 * - `flow-def`：流程定义 `DRAFT|PUBLISHED|DISABLED`（**停用叫 DISABLED，不是 OFFLINE**，
 *   与 D1 的 `dto::DEF_STATUSES` 逐字一致）；
 * - `flow-instance`：审批实例 `RUNNING|APPROVED|REJECTED|WITHDRAWN|VOID|CANCELLED`（PRD §3.3.1）；
 * - `flow-node`：节点实例 `PENDING|ACTIVE|APPROVED|REJECTED|SKIPPED|TERMINATED`（PRD §3.4）；
 * - `flow-task`：审批任务 `PENDING|APPROVED|REJECTED|SKIPPED|TERMINATED|TRANSFERRED|DELEGATED`（PRD §3.5）。
 *
 * 实例侧接口由 D2 交付，这里先按 PRD 的状态机落表 —— D2 上线若改了字面量，
 * 未知状态会走兜底显示原值，不会白屏（见 {@link statusMeta}）。
 */
const FLOW_DEF_STATUS_MAP: Record<string, StatusMeta> = {
  DRAFT: { label: '草稿', theme: 'default', dotVar: 'var(--td-text-color-placeholder)' },
  PUBLISHED: { label: '已发布', theme: 'success', dotVar: 'var(--td-success-color)' },
  DISABLED: { label: '已停用', theme: 'default', dotVar: 'var(--td-text-color-disabled)' },
};

/** 版本状态（`PUBLISHED` 之外还有 `ARCHIVED`：被新版本取代的旧发布版） */
const FLOW_VERSION_STATUS_MAP: Record<string, StatusMeta> = {
  DRAFT: { label: '草稿', theme: 'default', dotVar: 'var(--td-text-color-placeholder)' },
  PUBLISHED: { label: '已发布', theme: 'success', dotVar: 'var(--td-success-color)' },
  ARCHIVED: { label: '已归档', theme: 'default', dotVar: 'var(--td-text-color-disabled)' },
};

/** 审批实例状态 */
const FLOW_INSTANCE_STATUS_MAP: Record<string, StatusMeta> = {
  RUNNING: { label: '审批中', theme: 'primary', dotVar: 'var(--td-brand-color)' },
  // 架构 §10.4.1 的旧样例用的是 PENDING，兼容一下（后端若按旧样例实现也不会显示成灰）
  PENDING: { label: '审批中', theme: 'primary', dotVar: 'var(--td-brand-color)' },
  APPROVED: { label: '已通过', theme: 'success', dotVar: 'var(--td-success-color)' },
  REJECTED: { label: '已驳回', theme: 'danger', dotVar: 'var(--td-error-color)' },
  WITHDRAWN: { label: '已撤回', theme: 'default', dotVar: 'var(--td-text-color-disabled)' },
  VOID: { label: '已作废', theme: 'default', dotVar: 'var(--td-text-color-disabled)' },
  CANCELLED: { label: '已取消', theme: 'default', dotVar: 'var(--td-text-color-disabled)' },
};

/** 节点实例状态 */
const FLOW_NODE_STATUS_MAP: Record<string, StatusMeta> = {
  PENDING: { label: '未到达', theme: 'default', dotVar: 'var(--td-text-color-placeholder)' },
  // 术语锁定（P-FLOW-1C 决策 4）：节点 ACTIVE 是「处理中」不是「审批中」——
  // 这个节点可能是 TASK_NODE（在等一张任务干完），说「审批中」会让干活的人以为要找谁签字
  ACTIVE: { label: '处理中', theme: 'primary', dotVar: 'var(--td-brand-color)' },
  APPROVED: { label: '已通过', theme: 'success', dotVar: 'var(--td-success-color)' },
  REJECTED: { label: '已驳回', theme: 'danger', dotVar: 'var(--td-error-color)' },
  SKIPPED: { label: '已跳过', theme: 'default', dotVar: 'var(--td-text-color-disabled)' },
  TERMINATED: { label: '已中止', theme: 'warning', dotVar: 'var(--td-warning-color)' },
};

/** 审批任务状态 */
const FLOW_TASK_STATUS_MAP: Record<string, StatusMeta> = {
  PENDING: { label: '待处理', theme: 'primary', dotVar: 'var(--td-brand-color)' },
  APPROVED: { label: '已同意', theme: 'success', dotVar: 'var(--td-success-color)' },
  REJECTED: { label: '已驳回', theme: 'danger', dotVar: 'var(--td-error-color)' },
  SKIPPED: { label: '已跳过', theme: 'default', dotVar: 'var(--td-text-color-disabled)' },
  TERMINATED: { label: '已中止', theme: 'warning', dotVar: 'var(--td-warning-color)' },
  TRANSFERRED: { label: '已转办', theme: 'default', dotVar: 'var(--td-text-color-secondary)' },
  DELEGATED: { label: '已委托', theme: 'default', dotVar: 'var(--td-text-color-secondary)' },
};

/** 抄送阅读状态（`CcRes.is_read` 是布尔，这里映射成两个字面量给徽标用） */
const FLOW_CC_STATUS_MAP: Record<string, StatusMeta> = {
  UNREAD: { label: '未读', theme: 'primary', dotVar: 'var(--td-brand-color)' },
  READ: { label: '已读', theme: 'default', dotVar: 'var(--td-text-color-placeholder)' },
};

/**
 * 合同状态映射（A5 · `contract_model::STATUSES` 逐字一致）。
 *
 * 配色口径来自 A5 卡面：草稿灰 / 生效中绿 / 已完成蓝 / 已终止橙；
 * **没有「已取消」**——草稿作废走软删除，卡面写的 `CANCELLED` 在 DDL 的
 * `ck_biz_contract_status` 白名单里不存在（A2 已报告过同一处出入）。
 *
 * `SIGNED`（已签订、待生效）卡面没给颜色：`t-tag` 只有 5 个 theme，绿被 `ACTIVE`、
 * 蓝被 `COMPLETED`、橙被 `TERMINATED` 占了，于是按「尚未生效」的口径留在灰档，
 * 用更深的圆点色（`text-color-secondary`）与 `DRAFT` 的浅灰区分。
 */
const ERP_CONTRACT_STATUS_MAP: Record<string, StatusMeta> = {
  DRAFT: { label: '草稿', theme: 'default', dotVar: 'var(--td-text-color-placeholder)' },
  // 审批中（R-P1-1）：与采购单 SUBMITTED、流程待办 PENDING 同一套语义/配色（进行中 = 品牌蓝）
  PENDING_APPROVAL: { label: '审批中', theme: 'primary', dotVar: 'var(--td-brand-color)' },
  SIGNED: { label: '已签订', theme: 'default', dotVar: 'var(--td-text-color-secondary)' },
  ACTIVE: { label: '已生效', theme: 'success', dotVar: 'var(--td-success-color)' },
  COMPLETED: { label: '已完成', theme: 'primary', dotVar: 'var(--td-brand-color)' },
  TERMINATED: { label: '已终止', theme: 'warning', dotVar: 'var(--td-warning-color)' },
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
  'erp-contract': ERP_CONTRACT_STATUS_MAP,
  'flow-def': FLOW_DEF_STATUS_MAP,
  'flow-instance': FLOW_INSTANCE_STATUS_MAP,
  'flow-node': FLOW_NODE_STATUS_MAP,
  'flow-task': FLOW_TASK_STATUS_MAP,
  'flow-cc': FLOW_CC_STATUS_MAP,
};

/** 版本状态徽标（`flow-def` 域之外单独一套：`ARCHIVED` 只出现在版本列表里） */
export function flowVersionStatusMeta(status: string): StatusMeta {
  const key = (status ?? '').toString().trim().toUpperCase();
  return FLOW_VERSION_STATUS_MAP[key] ?? { ...FALLBACK_META, label: key || FALLBACK_META.label };
}

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
