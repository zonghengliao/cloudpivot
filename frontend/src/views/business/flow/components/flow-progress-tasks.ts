/**
 * `FlowProgress` 的「审批人明细」纯逻辑（P1-FIX-2）。
 *
 * 为什么单独一个 `.ts`：这些判据（哪个节点挂哪些人、空节点要不要显示、状态怎么配色）
 * 是**全站单据详情共用的口径**，但它跑在 `node` 里没有 DOM —— 只有拆成纯函数，
 * `pnpm test:unit`（`tsx --test`，零新依赖）才能直接打；`.vue` 只负责把结果画出来。
 *
 * 谁在用：`FlowProgress.vue`（渲染） + 采购订单详情（传 `timeline`）。
 */
import { statusMeta } from '@/constants/status-map';

/**
 * 进度条要的最小节点形状（详情页把 `NodeProgressRes` 映射成它）。
 *
 * 类型定义放在这个 `.ts` 里（而不是 `.vue`），是为了让「纯逻辑 + 组件」两边拿到
 * **同一份** 类型：`.ts` 反向 `import type ... from './FlowProgress.vue'` 在
 * 只装了 `*.vue` 通配 shim 的 tsc 下解析不出来（vue-tsc 才行）。
 */
export interface FlowProgressNode {
  /** 节点 key */
  node_key: string;
  /** 节点中文名 */
  name: string;
  /** 节点类型 */
  type: string;
  /** 轮次 */
  round: number;
  /** 节点实例状态 */
  status: string;
  /** 应到人数（会签） */
  approver_count?: number;
  /** 已同意（会签） */
  approved_count?: number;
  /** 耗时（毫秒，0/缺省不显示） */
  duration_ms?: number;
}

/** 审批人明细行的最小形状（详情页把 `TaskRes` / `FlowTaskItem` 映射成它） */
export interface FlowProgressTask {
  /** 节点 key（用来把明细挂到对应节点下面） */
  node_key: string;
  /** 轮次 */
  round: number;
  /** 节点中文名（节点列表里没有这个 key 时兜底显示） */
  node_name?: string;
  /** 审批人 id（可选，仅用于渲染 key） */
  assignee_id?: string;
  /** 审批人姓名（后端已删 → 空串，这里显示「未指派」） */
  assignee_name: string;
  /** 任务状态 `PENDING|APPROVED|REJECTED|SKIPPED|TERMINATED|TRANSFERRED|DELEGATED` */
  status: string;
  /** 时间（完成时间优先，没有就到岗时间） */
  at?: string | null;
  /** 审批意见 */
  comment?: string;
  /** 系统代点 */
  is_auto?: boolean;
  /** 自审 */
  is_self_approval?: boolean;
  /** 升级留痕：原本该审的角色编码（P3-02；非空 = 这条待办是升级派下来的） */
  escalated_from_role?: string;
  /** 升级原因 `SKIP_SELF_APPROVAL` / `NO_MEMBER`（P3-02） */
  escalate_reason?: string;
  /** 升级目标角色名（P3-02，展示用） */
  escalated_to_role?: string;
}

/** 一组「节点 → 该节点下的审批人」 */
export interface FlowProgressTaskGroup {
  /** 节点 key */
  node_key: string;
  /** 轮次 */
  round: number;
  /** 节点中文名（节点表里没有这个 key 时回落到任务自带的 node_name / node_key） */
  name: string;
  /** 该节点下的审批人（按后端 timeline 到达顺序，不排序） */
  tasks: FlowProgressTask[];
}

/** 节点 + 任务的分组 key（同一节点多轮用 `round` 区分） */
export function progressKey(nodeKey: string, round: number): string {
  return `${nodeKey}-${round ?? 1}`;
}

/**
 * 进度条节点的**显示顺序**（只看类型，不查流程图 —— 不额外打接口）。
 *
 * 为什么需要：后端 `NodeProgressRes` 的排序是 `round, created_at, node_key`，
 * 同一秒创建的节点按 key 排 —— 真接口里就出现过
 * 「金额分级① → 部门主管审批 → 开始 → 结束」这种把「开始」画到最后的情况
 * （`PO-20261031-000002` 的 FLW-20261101-02524DBF 实测）。
 *
 * 规则（稳定排序，中间节点保持引擎给的顺序）：
 * 1. `round` 升序（退回后的新一轮排在后面）；
 * 2. `START` 永远第一；
 * 3. `END` 永远最后；
 * 4. 其余保持原序。
 */
export function orderProgressNodes(
  nodes: readonly FlowProgressNode[],
): FlowProgressNode[] {
  return nodes
    .map((node, index) => ({ node, index }))
    .sort((a, b) => {
      const roundDiff = (a.node.round ?? 1) - (b.node.round ?? 1);
      if (roundDiff !== 0) {
        return roundDiff;
      }
      const rank = (node: FlowProgressNode): number => {
        if (node.type === 'START') {
          return 0;
        }
        if (node.type === 'END') {
          return 2;
        }
        return 1;
      };
      const rankDiff = rank(a.node) - rank(b.node);
      return rankDiff !== 0 ? rankDiff : a.index - b.index;
    })
    .map((item) => item.node);
}

/**
 * 把审批人明细挂到对应节点下面。
 *
 * 规则（三条都是踩过的坑）：
 * 1. **节点顺序跟着 `nodes` 走** —— 进度条是从左往右读的，明细列表倒过来读很别扭；
 * 2. **没有审批人的节点不出组** —— 「开始」「金额分级①」这类自动通过节点只有节点行
 *    没有待办行，给它们列空标题只会把抽屉拉长；
 * 3. **节点表里找不到的 key 也要显示** —— 退回后旧轮次的任务行仍在 timeline 里，
 *    节点表只给当前轮，丢了就等于把「上一轮谁驳回的」藏起来（自审/驳回依据）。
 */
export function groupProgressTasks(
  nodes: readonly FlowProgressNode[],
  tasks: readonly FlowProgressTask[] | null | undefined,
): FlowProgressTaskGroup[] {
  const groups = new Map<string, FlowProgressTaskGroup>();
  for (const node of nodes) {
    groups.set(progressKey(node.node_key, node.round), {
      node_key: node.node_key,
      round: node.round,
      name: node.name,
      tasks: [],
    });
  }
  for (const task of tasks ?? []) {
    const key = progressKey(task.node_key, task.round);
    let group = groups.get(key);
    if (!group) {
      group = {
        node_key: task.node_key,
        round: task.round,
        name: task.node_name || task.node_key,
        tasks: [],
      };
      groups.set(key, group);
    }
    group.tasks.push(task);
  }
  return [...groups.values()].filter((group) => group.tasks.length > 0);
}

/**
 * 审批人那一行的状态 class（配色口径）。
 *
 * | 状态 | class | 视觉 |
 * |---|---|---|
 * | `APPROVED` | `is-approved` | 绿点 + 绿字 |
 * | `REJECTED` | `is-rejected` | 红点 + 红字 |
 * | `PENDING` | `is-pending` | 空心点（还没表态） |
 * | `SKIPPED` / `TERMINATED` | `is-closed` | 半透明（这人不审了） |
 * | `TRANSFERRED` / `DELEGATED` / 其它 | — | 常态（留痕但不是终态） |
 */
export function progressTaskClass(status: string): Record<string, boolean> {
  return {
    'is-approved': status === 'APPROVED',
    'is-rejected': status === 'REJECTED',
    'is-pending': status === 'PENDING',
    'is-closed': status === 'SKIPPED' || status === 'TERMINATED',
  };
}

/** 审批人状态文案（`APPROVED` → 已同意；未知状态原样显示） */
export function progressTaskLabel(status: string): string {
  return statusMeta('flow-task', status).label;
}

/**
 * 节点状态 class（`FlowProgress` 进度条的配色口径）。
 *
 * 与 `progressTaskClass` 分成两份是**故意的**：节点集合
 * （`PENDING|ACTIVE|APPROVED|REJECTED|SKIPPED|TERMINATED`）和任务集合
 * （多 `TRANSFERRED|DELEGATED`、没有 `ACTIVE`）本来就不一样，混用会漏配色。
 */
export function progressNodeClass(status: string): Record<string, boolean> {
  return {
    'is-done': status === 'APPROVED',
    'is-skipped': status === 'SKIPPED' || status === 'TERMINATED',
    'is-active': status === 'ACTIVE',
    'is-rejected': status === 'REJECTED',
    'is-pending': status === 'PENDING' || !status,
  };
}

/** 节点状态文案（`APPROVED` → 已通过、`ACTIVE` → 处理中；未知状态原样显示） */
export function progressNodeLabel(status: string): string {
  return statusMeta('flow-node', status).label;
}

/**
 * 升级原因 → 人读标签（P3-02）。
 *
 * 码值由后端 `biz_flow_task.escalate_reason` 给出，**改码即破坏契约**：
 * `SKIP_SELF_APPROVAL` = 自审策略把审批人全剔了；`NO_MEMBER` = 角色本来就没人。
 */
export function escalateReasonLabel(reason?: string): string {
  switch (reason) {
    case 'SKIP_SELF_APPROVAL':
      return '因自审跳过';
    case 'NO_MEMBER':
      return '因角色无成员';
    default:
      return '因无可用审批人';
  }
}

/**
 * 待办行上的升级提示（空串 = 这条待办不是升级派下来的）。
 *
 * 形如 `⚠ 因自审跳过，已升级至「销售总监」审批（原角色 SALES_CLERK）`：
 * - 目标角色名来自后端 `escalated_to_role`（即 `assignee_source_ref` 指向的角色）；
 * - 原角色编码来自 `escalated_from_role`，方便管理员一眼看出「该给谁加人」。
 */
export function escalationHint(task: FlowProgressTask): string {
  if (!task.escalate_reason) {
    return '';
  }
  const target = task.escalated_to_role ? `「${task.escalated_to_role}」` : '上级角色';
  const origin = task.escalated_from_role ? `（原角色 ${task.escalated_from_role}）` : '';
  return `⚠ ${escalateReasonLabel(task.escalate_reason)}，已升级至${target}审批${origin}`;
}
