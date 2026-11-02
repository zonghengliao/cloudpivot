/**
 * 单据详情里的「审批进度」装载逻辑（P1-FIX-2，采购订单）。
 *
 * 口径**照抄收货单（P16）/ 应付单（P17）**：详情接口只给 `approval_instance_id`，
 * 前端再调 `GET /business/flow/instance/{id}` 拿 `nodes[]`，交给 `FlowProgress` 画。
 *
 * 三条硬口径：
 * 1. **没有实例 id 就什么都不显示**（草稿 / 已驳回打回的单据不挂实例，别画个空标题）；
 * 2. **拉不到节点不报错、不弹窗**，退化成「审批单号链接」这一行老入口 ——
 *    详情页不能因为审批中心抽风就整块失败；
 * 3. **进度只来自引擎写的节点行**，前端不自己推「走到第几步」。
 *
 * 纯逻辑全在这里（无 Vue 依赖）→ `pnpm test:unit` 直接打。
 */
import type { FlowNodeProgress, FlowTaskItem } from '@/api/business/flow-api';

import type {
  FlowProgressNode,
  FlowProgressTask,
} from '@/views/business/flow/components/flow-progress-tasks';

/** 详情抽屉里这一块的标题（与收货单 / 应付单同一个词） */
export const APPROVAL_PROGRESS_TITLE = '审批进度';

/** 审批进度装载结果（永远返回它，不抛异常） */
export interface ApprovalProgress {
  /** 节点进度（空 = 画不出来） */
  nodes: FlowProgressNode[];
  /** 审批人明细（空 = 只有进度条，或者连进度条都没有） */
  tasks: FlowProgressTask[];
}

/** 空的装载结果（每次重置都用同一份，别让上一次的节点漏到下一张单） */
export const EMPTY_APPROVAL_PROGRESS: ApprovalProgress = { nodes: [], tasks: [] };

/**
 * `NodeProgressRes[]` → `FlowProgressNode[]`。
 *
 * `node_name` 是**唯一**带中文名的地方（列表接口没有），空就回落到 `node_key` ——
 * 宁可显示 `n_dept` 也不要显示空白。
 */
export function mapApprovalNodes(
  nodes: readonly FlowNodeProgress[] | null | undefined,
): FlowProgressNode[] {
  return (nodes ?? []).map((node) => ({
    node_key: node.node_key,
    name: node.node_name || node.node_key,
    type: node.node_type,
    round: node.round,
    status: node.status,
    approver_count: node.approver_count ?? 0,
    approved_count: node.approved_count ?? 0,
    duration_ms: node.duration_ms ?? undefined,
  }));
}

/**
 * `TaskRes[]`（实例详情的 `timeline`）→ `FlowProgressTask[]`（审批人明细）。
 *
 * - **时间取「完成时间优先、没有就到岗时间」**：待处理的人 ended_at 是空的，
 *   显示到岗时间才能看出「这单在他手上多久了」；
 * - `SKIPPED`（跳过）/ `TERMINATED`（中止）也保留 —— 那是留痕，藏起来就看不出
 *   「这个节点本来该谁审、为什么没审」；
 * - 姓名空（后端用户已删）原样给空串，由组件显示「未指派」。
 */
export function mapApprovalTasks(
  timeline: readonly FlowTaskItem[] | null | undefined,
): FlowProgressTask[] {
  return (timeline ?? []).map((task) => ({
    node_key: task.node_key,
    round: task.round,
    node_name: task.current_node_name,
    assignee_id: task.assignee_id,
    assignee_name: task.assignee_name,
    status: task.status,
    at: task.completed_at || task.created_at || null,
    comment: task.comment || '',
    is_auto: task.is_auto,
    is_self_approval: task.is_self_approval,
  }));
}

/**
 * 拉审批进度。**永不抛异常**：
 * - 没有实例 id → 直接空结果（不发请求）；
 * - 请求失败（权限 / 实例已归档 / 网络）→ 空结果，页面降级成单号链接。
 *
 * @param instanceId 单据上的 `approval_instance_id`（弱引用，可能为 null）
 * @param fetchInstance `getFlowInstance`（测试注入假实现）
 */
export async function loadApprovalProgress(
  instanceId: string | null | undefined,
  fetchInstance: (id: string) => Promise<{
    nodes?: readonly FlowNodeProgress[] | null;
    timeline?: readonly FlowTaskItem[] | null;
  }>,
): Promise<ApprovalProgress> {
  if (!instanceId) {
    return { ...EMPTY_APPROVAL_PROGRESS };
  }
  try {
    const instance = await fetchInstance(instanceId);
    return {
      nodes: mapApprovalNodes(instance.nodes),
      tasks: mapApprovalTasks(instance.timeline),
    };
  } catch {
    return { ...EMPTY_APPROVAL_PROGRESS };
  }
}

/** 降级入口文案（拉不到节点时显示的那一行「审批中 FLW-xxx」） */
export interface ApprovalFallback {
  /** 前缀：`审批中` / `审批单` */
  label: string;
  /** 单号（没有单号就退到实例 id） */
  no: string;
}

/**
 * 拉不到节点时，那一行单号链接该写什么。
 *
 * - `SUBMITTED`（审批中）→「审批中 FLW-xxx」，与列表口径一致；
 * - 其它状态（已通过 / 已驳回 / 已作废）→「审批单 FLW-xxx」，别再说审批中；
 * - **没有实例 id → `null`**（什么都不显示，连链接都没有）。
 */
export function approvalFallbackOf(detail: {
  status?: string | null;
  approval_instance_id?: string | null;
  approval_instance_no?: string | null;
} | null | undefined): ApprovalFallback | null {
  const instanceId = detail?.approval_instance_id;
  if (!instanceId) {
    return null;
  }
  return {
    label: detail?.status === 'SUBMITTED' ? '审批中' : '审批单',
    no: detail?.approval_instance_no || instanceId,
  };
}

/** 进度区块要不要渲染（有节点才渲染；拉不到就交给降级链接） */
export function hasApprovalProgress(progress: ApprovalProgress | null | undefined): boolean {
  return (progress?.nodes.length ?? 0) > 0;
}
