/**
 * Vue Flow 画布的节点视图模型（纯逻辑，不含 Vue / DOM）。
 *
 * # 为什么单独一层
 *
 * 节点卡片上显示的「图标 / 主标题 / 副标题 / 状态点 / class 集合」是一套**业务文案规则**
 * （会签模式、缺审批人、任务完成方式、运行状态叠加…），与「谁来画」无关。
 * 抽成纯函数后：
 * - 单测能直接断言文案（不用挂载组件）；
 * - 画布组件只剩模板与交互；
 * - 老画布 `FlowCanvas.vue` 里那份内联实现**本卡一行不改**（保留回退），
 *   等 VUE-FLOW-3 删掉老画布时，这里自然成为唯一实现。
 *
 * ⚠ 这两份实现的一致性靠 `tests/unit/flow-canvas-vueflow-contract.test.ts` 的文案用例钉住
 * （同一份输入必须产出同一段副标题）；改文案时两边都要动，否则用例红。
 *
 * # 颜色纪律
 *
 * 本文件不出现任何颜色字面量：类型色全部由 CSS class + `--node-accent` 表达。
 */
import type { FlowNode } from '@/api/business/flow-api';
import { NODE_META } from '@/views/business/flow/graph';
import { needsAssigneeOf } from '@/views/business/flow/components/flow-node-caps';
import { ON_COMPLETE_LABELS, type OnComplete } from '@/views/business/flow/components/task-node-config';

/** 状态点档位（决定底色） */
export type StatusTone = 'approved' | 'active' | 'pending' | 'rejected';

/** 节点在画布上的视图数据（挂进 Vue Flow 的 `node.data`） */
export interface FlowNodeViewData {
  /** 节点 key（渲染层不再从 data 反推，测试与调试都用它） */
  key: string;
  /** 节点类型（`data-node-type` 用） */
  type: string;
  /** 左上角图标字 */
  icon: string;
  /** 主文案 */
  label: string;
  /** 副文案（空串 = 不显示） */
  subLabel: string;
  /** 状态点内容（空串 = 只画底色圈） */
  statusMark: string;
  /** 是否有运行状态 */
  hasStatus: boolean;
  /** 状态点配色档 */
  statusTone: StatusTone;
  /** 卡片宽（画布逻辑像素） */
  w: number;
  /** 卡片高 */
  h: number;
  /** class 集合 */
  cls: Record<string, boolean>;
}

/** 节点视图的渲染上下文（画布把 props 归一后传进来） */
export interface FlowNodeViewContext {
  /** 当前选中的节点 key */
  selectedKey?: string | null;
  /** 校验命中的节点 key 集合 */
  issueKeys?: Set<string>;
  /** 节点运行状态表 */
  status?: Record<string, string>;
  /** 正在拉线、且当前落点悬在该节点上（老画布的 is-connect-target 提示） */
  connectTargetKey?: string | null;
}

/** 类型 → 左上角图标字（★ 只影响观感，不参与任何判定，与老画布逐字一致） */
export const NODE_GLYPH: Record<string, string> = {
  START: '▶',
  APPROVAL: '◉',
  COUNTERSIGN: '⊕',
  CONDITION: '◇',
  PARALLEL: '∥',
  CC: '↗',
  END: '■',
  TASK_NODE: '☑',
  NOTIFY_NODE: '✉',
};

/** 运行状态 → 中文（副标题 / 状态点共用） */
export const STATUS_TEXT: Record<string, string> = {
  APPROVED: '已通过',
  // 术语锁定（决策 4）：节点 ACTIVE 显示「处理中」，不是「审批中」—— 节点可能是任务节点
  ACTIVE: '处理中',
  REJECTED: '已驳回',
  SKIPPED: '已跳过',
  WAITING: '待处理',
  PENDING: '待处理',
};

/** 运行状态 → 状态点档位 */
export const STATUS_TONE: Record<string, StatusTone> = {
  APPROVED: 'approved',
  ACTIVE: 'active',
  REJECTED: 'rejected',
  SKIPPED: 'pending',
  WAITING: 'pending',
  PENDING: 'pending',
};

/** 运行状态 → 状态点内容（进行中 / 待处理留空，靠底色和脉冲环表达） */
export const STATUS_MARK: Record<string, string> = {
  APPROVED: '✓',
  ACTIVE: '',
  REJECTED: '✕',
  SKIPPED: '',
  WAITING: '',
  PENDING: '',
};

/**
 * 算副标题：业务信息优先（会签模式 / 缺审批人 / 任务完成方式），其次运行状态。
 *
 * # 参数
 * - `node`：节点
 * - `status`：该节点的运行状态（可空）
 */
export function nodeSubLabel(node: FlowNode, status?: string): string {
  const isCountersign = node.type === 'COUNTERSIGN';
  const missingAssignee = needsAssigneeOf(node.type) && !node.assignee_type;
  let subLabel = '';
  if (isCountersign && node.approval_mode) {
    subLabel =
      node.approval_mode === 'RATIO'
        ? `按比例 ${Math.round((node.approve_ratio ?? 0) * 100)}%`
        : node.approval_mode === 'ANY'
          ? '或签'
          : '全部同意';
  } else if (missingAssignee) {
    subLabel = node.type === 'TASK_NODE' ? '未配置负责人' : '未配置审批人';
  } else if (node.type === 'TASK_NODE') {
    // 任务节点：把「完成后怎么走」直接画在卡片上（与属性面板同一份文案）
    subLabel = ON_COMPLETE_LABELS[(node.config as { task?: { on_complete?: string } })?.task?.on_complete as OnComplete] ?? '生成任务';
  } else if (node.type === 'NOTIFY_NODE') {
    subLabel = '发通知（发完即通过）';
  } else if (node.type === 'CONDITION') {
    subLabel = '分支网关';
  } else if (node.type === 'PARALLEL') {
    subLabel = node.fork_mode === 'OR' ? '或分叉' : '并行';
  } else if (node.type === 'CC') {
    subLabel = '只读知会';
  } else if (node.type === 'START' || node.type === 'END') {
    subLabel = node.type === 'START' ? '入口' : '出口';
  }
  const statusText = status ? STATUS_TEXT[status] : '';
  if (statusText) {
    subLabel = subLabel ? `${subLabel} · ${statusText}` : statusText;
  }
  return subLabel;
}

/** 取节点尺寸（未知类型兜底审批尺寸，与连线口径同一份） */
export function nodeSizeOf(type: string): { w: number; h: number } {
  return NODE_META[type]?.size ?? NODE_META.APPROVAL.size;
}

/**
 * 判断一条边是否「已经走过」（进度图高亮）。
 *
 * `takenPairs` 的元素格式是 **`${from}>${to}`**（后端 `FlowNodeProgress.taken_edge_key`
 * / 前端 `takenPairs` 都是这个口径）。抽成函数是为了让单测能直接钉住这个格式 ——
 * 一旦有人把分隔符写成 `->`，进度图上「走过的边」会整片不亮，而画布本身不会报错。
 */
export function isEdgeTaken(takenPairs: Iterable<string> | undefined | null, from: string, to: string): boolean {
  if (!takenPairs) {
    return false;
  }
  const key = `${from}>${to}`;
  for (const pair of takenPairs) {
    if (pair === key) {
      return true;
    }
  }
  return false;
}

/**
 * 把一个 `FlowNode` 变成渲染用视图数据。
 *
 * # 参数
 * - `node`：节点
 * - `ctx`：选中 / 校验 / 运行状态 / 连线落点
 */
export function buildNodeView(node: FlowNode, ctx: FlowNodeViewContext = {}): FlowNodeViewData {
  const size = nodeSizeOf(node.type);
  const status = ctx.status?.[node.key];
  return {
    key: node.key,
    type: node.type,
    icon: NODE_GLYPH[node.type] ?? NODE_GLYPH.APPROVAL,
    label: (node.name || NODE_META[node.type]?.label || node.type).trim(),
    subLabel: nodeSubLabel(node, status),
    statusMark: status ? (STATUS_MARK[status] ?? '') : '',
    hasStatus: Boolean(status),
    statusTone: status ? (STATUS_TONE[status] ?? 'pending') : 'pending',
    w: size.w,
    h: size.h,
    cls: {
      'is-selected': ctx.selectedKey === node.key,
      'has-issue': Boolean(ctx.issueKeys?.has(node.key)),
      // 高度 44 的胶囊（开始 / 结束）放不下图标 + 主副两行，切紧凑排版
      'is-compact': size.h < 56,
      'is-start': node.type === 'START',
      'is-end': node.type === 'END',
      'is-approval': node.type === 'APPROVAL',
      'is-countersign': node.type === 'COUNTERSIGN',
      'is-condition': node.type === 'CONDITION',
      'is-parallel': node.type === 'PARALLEL',
      'is-cc': node.type === 'CC',
      'is-task': node.type === 'TASK_NODE',
      'is-notify': node.type === 'NOTIFY_NODE',
      'is-active': status === 'ACTIVE',
      'is-approved': status === 'APPROVED',
      'is-rejected': status === 'REJECTED',
      'is-skipped': status === 'SKIPPED' || status === 'WAITING',
      'is-connect-target': ctx.connectTargetKey === node.key,
    },
  };
}
