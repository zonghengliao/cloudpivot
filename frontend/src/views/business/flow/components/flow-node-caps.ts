/**
 * 节点能力表（P-FLOW-1C · 设计 §4.1「关键交互纪律」第 4 条）。
 *
 * # 这个文件解决什么
 *
 * `NodeProps.vue` 原来用 4 个 `supportsXxx` computed 按类型分支，最长的那个是
 * `type === 'APPROVAL' || type === 'COUNTERSIGN' || type === 'CC'`。
 * 再加 `TASK_NODE` / `NOTIFY_NODE` 就是第 5、6 处 `||` —— 每加一种节点类型要改 4~6 个地方，
 * 漏一处就是「节点能拖进来但属性面板什么都没有」。
 *
 * 现在**只有这一张表**：新增节点类型 = 加一个表项，`NodeProps.vue` / `FlowCanvas.vue`
 * / 左侧面板都读它。表是纯数据，单测直接断言整张表。
 *
 * # 与后端的对应关系
 *
 * | 字段 | 后端依据 |
 * |---|---|
 * | `editable` | `graph::NodeType` 里 START/END 不可编辑（固定出入口） |
 * | `canHaveAssignee` | `graph.rs` R14：`APPROVAL` / `COUNTERSIGN` 必须配审批人；`TASK_NODE` 同理（R20） |
 * | `canHaveTimeout` | `dto::TIMEOUT_ACTIONS` 只对需要等人的节点有意义 |
 * | `canHaveConditionOut` | 条件节点的出边才允许带 `condition`（`graph.rs` 校验） |
 * | `blocksOnHuman` | 画布配色：需要人等 → 实心强调色；网关/知会 → 虚线 |
 * | `needsConfig` | `config.*` 分区：`task`（§2.3）/ `notify`（§2.6）/ `expression`（EXPRESSION 指派） |
 */

/** 十种节点（与后端 `graph::NodeType` 逐字对应；新增两种在末尾） */
export type NodeType =
  | 'START'
  | 'APPROVAL'
  | 'COUNTERSIGN'
  | 'CONDITION'
  | 'PARALLEL'
  | 'CC'
  | 'END'
  | 'TASK_NODE'
  | 'NOTIFY_NODE'
  | 'SUBPROCESS';

/** 一种节点的能力 */
export interface NodeCaps {
  /** 属性是否可编辑（START / END 固定） */
  editable: boolean;
  /** 是否需要审批人 / 负责人 */
  canHaveAssignee: boolean;
  /** 是否支持超时策略 */
  canHaveTimeout: boolean;
  /** 出边是否可带条件（只有条件节点） */
  canHaveConditionOut: boolean;
  /** 是否阻塞等人（画布配色 + 副标题用） */
  blocksOnHuman: boolean;
  /** `config` 分区：`null` = 无 */
  needsConfig: null | 'task' | 'notify' | 'expression' | 'sub';
}

/** 能力表（**唯一来源**，逐行照设计 §4.1 的表） */
export const CAPS: Record<NodeType, NodeCaps> = {
  START: {
    editable: false,
    canHaveAssignee: false,
    canHaveTimeout: false,
    canHaveConditionOut: false,
    blocksOnHuman: false,
    needsConfig: null,
  },
  APPROVAL: {
    editable: true,
    canHaveAssignee: true,
    canHaveTimeout: true,
    canHaveConditionOut: false,
    blocksOnHuman: true,
    needsConfig: null,
  },
  COUNTERSIGN: {
    editable: true,
    canHaveAssignee: true,
    canHaveTimeout: true,
    canHaveConditionOut: false,
    blocksOnHuman: true,
    needsConfig: null,
  },
  CONDITION: {
    editable: true,
    canHaveAssignee: false,
    canHaveTimeout: false,
    canHaveConditionOut: true,
    blocksOnHuman: false,
    needsConfig: null,
  },
  PARALLEL: {
    editable: true,
    canHaveAssignee: false,
    canHaveTimeout: false,
    canHaveConditionOut: false,
    blocksOnHuman: false,
    needsConfig: null,
  },
  CC: {
    editable: true,
    canHaveAssignee: true,
    canHaveTimeout: false,
    canHaveConditionOut: false,
    blocksOnHuman: false,
    needsConfig: null,
  },
  END: {
    editable: false,
    canHaveAssignee: false,
    canHaveTimeout: false,
    canHaveConditionOut: false,
    blocksOnHuman: false,
    needsConfig: null,
  },
  TASK_NODE: {
    editable: true,
    canHaveAssignee: true,
    canHaveTimeout: true,
    canHaveConditionOut: false,
    blocksOnHuman: true,
    needsConfig: 'task',
  },
  NOTIFY_NODE: {
    editable: true,
    canHaveAssignee: true,
    canHaveTimeout: true,
    canHaveConditionOut: false,
    blocksOnHuman: false,
    needsConfig: 'notify',
  },
  // ★ P3-08 / ARCH-5 §4.14：子流程节点 —— 一个节点里跑另一张完整的图。
  //   它**阻塞等人**（父节点 ACTIVE 等子实例结算），但没有审批人 / 超时概念：
  //   子流程的超时归子流程自己的节点管，父侧再配一份会两处打架。
  SUBPROCESS: {
    editable: true,
    canHaveAssignee: false,
    canHaveTimeout: false,
    canHaveConditionOut: false,
    blocksOnHuman: true,
    needsConfig: 'sub',
  },
};

/** 未知类型的兜底能力（后端新增类型时画布不白屏：按「可编辑、无配置」处理） */
const UNKNOWN_CAPS: NodeCaps = {
  editable: true,
  canHaveAssignee: false,
  canHaveTimeout: false,
  canHaveConditionOut: false,
  blocksOnHuman: false,
  needsConfig: null,
};

/**
 * 取某种节点的能力。
 *
 * 未知类型**不抛错**：发布出去的旧版本可能被新后端读取出新类型，
 * 编辑器要能打开它并显示「未知类型」（能力按 {@link UNKNOWN_CAPS}），而不是白屏。
 */
export function capabilitiesOf(type: string | null | undefined): NodeCaps {
  const key = (type ?? '').trim().toUpperCase() as NodeType;
  return CAPS[key] ?? UNKNOWN_CAPS;
}

/** 是否任务节点 */
export function isTaskNode(type: string | null | undefined): boolean {
  return (type ?? '').trim().toUpperCase() === 'TASK_NODE';
}

/** 是否通知节点 */
export function isNotifyNode(type: string | null | undefined): boolean {
  return (type ?? '').trim().toUpperCase() === 'NOTIFY_NODE';
}

/** 是否子流程节点（P3-08 / ARCH-5 §4.14） */
export function isSubprocessNode(type: string | null | undefined): boolean {
  return (type ?? '').trim().toUpperCase() === 'SUBPROCESS';
}

/** 是否需要审批人（画布「未配置审批人」提示 + 属性面板整块显隐共用） */
export function needsAssigneeOf(type: string | null | undefined): boolean {
  return capabilitiesOf(type).canHaveAssignee;
}

/**
 * 指派来源选项（设计 §4.3 的「唯一的新增」）。
 *
 * `TASK_NODE` 上去掉 `TASK_OWNER` / `TASK_CREATOR` / `TASK_ASSIGNER`：
 * 那三个依赖 `biz_type='TASK_ACCEPT'` 才有值，用在本流程上会**解析 0 人**。
 * 这里灰掉是**体验**，真正的闸是后端发布校验 R20 —— 前端灰掉不算校验。
 *
 * # 参数
 * - `type`：节点类型
 * - `bizType`：本流程业务类型（`TASK_ACCEPT` 时三个任务来源才可用）
 */
export function assigneeKindOptionsOf(
  type: string | null | undefined,
  bizType?: string | null,
): Array<{ label: string; value: string; disabled: boolean }> {
  const taskSources = ['TASK_OWNER', 'TASK_CREATOR', 'TASK_ASSIGNER'];
  const base = [
    { label: '发起人主管', value: 'INITIATOR_SUPERIOR' },
    { label: '发起人本人', value: 'INITIATOR' },
    { label: '指定人', value: 'USER' },
    { label: '指定角色', value: 'ROLE' },
    { label: '部门负责人', value: 'DEPT_LEADER' },
    { label: '任务负责人', value: 'TASK_OWNER' },
    { label: '任务创建人', value: 'TASK_CREATOR' },
    { label: '任务分派人', value: 'TASK_ASSIGNER' },
    { label: '表单字段里的人', value: 'FORM_VAR' },
    { label: '管理员', value: 'ADMIN' },
  ];
  const isTaskAccept = (bizType ?? '').trim().toUpperCase() === 'TASK_ACCEPT';
  const disableTaskSources = isTaskNode(type) && !isTaskAccept;
  return base.map((item) => ({
    ...item,
    disabled: disableTaskSources && taskSources.includes(item.value),
  }));
}

/**
 * 超时动作选项（`TASK_NODE` 只允许 `REMIND` / `ESCALATE`）。
 *
 * 后端理由（设计 §2.4）：`AUTO_APPROVE` 会让「没干的活」在超时后**算干完**，
 * `AUTO_REJECT` 会把「没干」写成「被拒」—— 审批节点上这只是流程效率问题，
 * 任务节点上是**内控事故**。所以发布校验 `65031` 拒绝，前端这里只是提前灰掉。
 */
export function timeoutActionOptionsOf(
  type: string | null | undefined,
): Array<{ label: string; value: string; disabled: boolean }> {
  const isTask = isTaskNode(type);
  return [
    { label: '仅提醒', value: 'REMIND', disabled: false },
    { label: '自动通过', value: 'AUTO_APPROVE', disabled: isTask },
    { label: '自动驳回', value: 'AUTO_REJECT', disabled: isTask },
    { label: '升级给上级', value: 'ESCALATE', disabled: false },
  ];
}
