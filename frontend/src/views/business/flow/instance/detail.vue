<template>
  <div class="flow-detail-page">
    <t-button variant="text" size="small" class="back" @click="router.back()">
      <template #icon><t-icon name="chevron-left" /></template>
      返回
    </t-button>

    <!-- 加载失败 / 无权查看 -->
    <t-card v-if="loadError" :bordered="false" class="card">
      <t-alert theme="error" :message="loadError">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadDetail()">重试</t-button>
        </template>
      </t-alert>
      <div class="detail-empty">
        <t-icon name="inbox" size="28px" />
        <p>暂无数据</p>
      </div>
    </t-card>

    <div v-else-if="loading" class="detail-loading">加载中…</div>

    <template v-else-if="detail">
      <!-- ==================== 头部 ==================== -->
      <t-card :bordered="false" class="card">
        <div class="head">
          <div class="head__main">
            <div class="head__title-row">
              <span class="head__title">{{ detail.instance.title }}</span>
              <StatusBadge domain="flow-instance" :status="detail.instance.status" />
              <t-tag v-if="detail.auto_passed_nodes > 0" size="small" theme="warning" variant="light">
                自动通过 {{ detail.auto_passed_nodes }} 个节点
              </t-tag>
            </div>
            <div class="head__meta">
              <span class="head__no">{{ detail.instance.instance_no }}</span>
              <span class="head__sep">·</span>
              <span>{{ detail.instance.flow_name }}</span>
              <span class="head__version">v{{ detail.instance.version_no }}</span>
              <span class="head__sep">·</span>
              <span>发起人 {{ detail.instance.initiator_name || '—' }}</span>
              <span class="head__sep">·</span>
              <span>{{ formatTime(detail.instance.started_at) }}</span>
              <span v-if="detail.instance.ended_at" class="head__sep">·</span>
              <span v-if="detail.instance.ended_at">结束于 {{ formatTime(detail.instance.ended_at) }}</span>
              <span v-if="detail.instance.round_total > 1" class="head__sep">·</span>
              <span v-if="detail.instance.round_total > 1">已退回 {{ detail.instance.round_total - 1 }} 次</span>
            </div>
          </div>
          <div class="head__actions">
            <t-button variant="outline" size="small" :loading="loading" @click="loadDetail()">刷新</t-button>
            <t-button
              v-if="detail.instance.biz_id"
              variant="outline"
              size="small"
              @click="router.push(`/task?keyword=${detail.instance.biz_no}`)"
            >
              关联业务 {{ detail.instance.biz_no }}
            </t-button>
          </div>
        </div>

        <!-- 进度条（只吃后端 nodes[].status，不前端推算） -->
        <div class="progress-wrap">
          <FlowProgress :nodes="progressNodes" />
        </div>

        <div class="cc-row">
          <span class="cc-row__label">抄送：</span>
          <template v-if="detail.cc.length > 0">
            <t-tag v-for="item in detail.cc" :key="item.id" size="small" variant="light" :theme="item.is_read ? 'default' : 'primary'">
              {{ item.node_key }}{{ item.is_read ? '（已读）' : '（未读）' }}
            </t-tag>
          </template>
          <span v-else class="cc-row__empty">无</span>
        </div>
      </t-card>

      <t-row :gutter="[12, 12]">
        <!-- ==================== 左：内容 + 时间线 ==================== -->
        <t-col :span="16">
          <t-card :bordered="false" class="card">
            <div class="section-title">审批内容</div>
            <div class="content">
              <p class="content__title">{{ detail.instance.title }}</p>
              <div v-if="formFields.length > 0" class="content__fields">
                <div v-for="field in formFields" :key="field.key" class="content__field">
                  <span class="content__field-key">{{ field.key }}</span>
                  <span class="content__field-value">{{ field.value }}</span>
                </div>
              </div>
              <p v-else class="content__empty">发起时未填写表单内容</p>
              <p v-if="detail.instance.result_comment" class="content__comment">
                结束说明：{{ detail.instance.result_comment }}
              </p>
            </div>
          </t-card>

          <t-card :bordered="false" class="card timeline-card">
            <div class="section-title">审批时间线</div>
            <div v-if="detail.timeline.length === 0" class="timeline-empty">暂无审批记录</div>
            <ul v-else class="timeline">
              <li v-for="(item, index) in timelineItems" :key="index" class="timeline__item">
                <span class="timeline__dot" :class="item.cls" />
                <div class="timeline__body">
                  <div class="timeline__head">
                    <span class="timeline__who">{{ item.who }}</span>
                    <span class="timeline__what">{{ item.what }}</span>
                    <t-tag v-if="item.isAuto" size="small" variant="outline">系统代点</t-tag>
                    <t-tag v-if="item.isSelf" size="small" variant="outline" theme="warning">自审</t-tag>
                  </div>
                  <div class="timeline__time">{{ formatTime(item.at) }}</div>
                  <div v-if="item.comment" class="timeline__comment">{{ item.comment }}</div>
                </div>
              </li>
            </ul>
          </t-card>
        </t-col>

        <!-- ==================== 右：节点明细 + 我的操作 ==================== -->
        <t-col :span="8">
          <t-card :bordered="false" class="card">
            <div class="section-title">节点进度</div>
            <div class="node-list">
              <div v-for="node in detail.nodes" :key="`${node.node_key}-${node.round}`" class="node-item">
                <div class="node-item__head">
                  <span class="node-item__name">{{ node.node_name }}</span>
                  <StatusBadge domain="flow-node" :status="node.status" />
                </div>
                <div class="node-item__meta">
                  <span>{{ nodeTypeText(node.node_type) }}</span>
                  <span v-if="node.round > 1">第 {{ node.round }} 轮</span>
                  <span v-if="node.approver_count > 0">
                    {{ node.approved_count }}/{{ node.approver_count }} 已表态
                    <template v-if="node.approval_mode === 'RATIO'">
                      （需 {{ Math.round(node.approve_ratio * 100) }}%）
                    </template>
                  </span>
                  <span v-if="node.duration_ms">耗时 {{ durationText(node.duration_ms) }}</span>
                </div>
              </div>
            </div>
          </t-card>

          <!-- 底部操作区：只有我是当前审批人才出现（后端下发 my_task 才算） -->
          <t-card v-if="detail.my_task" :bordered="false" class="card action-card">
            <div class="section-title">我的审批</div>
            <t-space direction="vertical" size="small" style="width: 100%">
              <!-- 审批意见：后端 `approve` 接口本就收 comment（会写进时间线），
                   之前界面没给输入框 → 同意这条路永远不留意见。补上（可空，不拦截提交）。 -->
              <t-textarea
                v-model="approveComment"
                class="action-comment"
                :maxlength="500"
                :autosize="{ minRows: 3, maxRows: 6 }"
                show-limit
                placeholder="审批意见（可选，会记入审批时间线）"
              />
              <t-button theme="primary" block :loading="approving" @click="onApprove">同意</t-button>
              <t-button theme="danger" variant="outline" block @click="openReject">驳回</t-button>
              <t-button variant="outline" block @click="openTransfer">转办</t-button>
              <t-button variant="outline" block @click="openAddSign">加签</t-button>
            </t-space>
            <p class="action-tip">驳回必须填写意见（≥5 字）；会签节点只记你这一票。</p>
          </t-card>

          <!-- 发起人可撤回（零人已审才允许，后端行级判定） -->
          <t-card v-else-if="canWithdraw" :bordered="false" class="card action-card">
            <div class="section-title">我可以</div>
            <t-button variant="outline" block :loading="withdrawing" @click="onWithdraw">撤回审批</t-button>
            <p class="action-tip">只有「还没有人审批」时才能撤回；已有人表态请联系管理员终止。</p>
          </t-card>
        </t-col>
      </t-row>

      <t-alert
        v-if="detail.instance.result_comment"
        class="card"
        theme="info"
        :message="`结束说明：${detail.instance.result_comment}`"
      />
    </template>

    <!-- ==================== 驳回抽屉 ==================== -->
    <t-drawer
      v-model:visible="rejectVisible"
      header="驳回审批"
      size="420px"
      :confirm-btn="{ content: '确认驳回', theme: 'danger', loading: rejecting }"
      @confirm="submitReject"
    >
      <t-form label-align="top">
        <t-form-item label="退回方式" required>
          <t-select v-model="rejectForm.returnTo" :options="returnOptions" />
        </t-form-item>
        <t-form-item label="驳回意见（至少 5 个字）" required>
          <t-textarea v-model="rejectForm.comment" :autosize="{ minRows: 4, maxRows: 8 }" placeholder="说明驳回原因" :maxlength="500" />
          <p class="action-tip">{{ rejectForm.comment.trim().length }}/5 字</p>
        </t-form-item>
      </t-form>
    </t-drawer>

    <!-- 转办抽屉 -->
    <t-drawer
      v-model:visible="transferVisible"
      header="转办"
      size="400px"
      :confirm-btn="{ content: '确认转办', loading: transferring }"
      @confirm="submitTransfer"
    >
      <t-form label-align="top">
        <t-form-item label="转办给" required>
          <UserSelect v-model="transferForm.toUserId" />
        </t-form-item>
        <t-form-item label="说明">
          <t-textarea v-model="transferForm.reason" :autosize="{ minRows: 3, maxRows: 6 }" />
        </t-form-item>
      </t-form>
    </t-drawer>

    <!-- 加签抽屉 -->
    <t-drawer
      v-model:visible="addSignVisible"
      header="加签"
      size="400px"
      :confirm-btn="{ content: '确认加签', loading: addingSign }"
      @confirm="submitAddSign"
    >
      <t-form label-align="top">
        <t-form-item label="加签人" required>
          <UserSelect v-model="addSignForm.userId" />
        </t-form-item>
      </t-form>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 审批详情（卡面页面 5 / `/flow/instance/{id}`）。
 *
 * # 真实出参（D2 `InstanceDetailRes`）
 *
 * ```json
 * { "instance": {...}, "nodes": [NodeProgressRes], "timeline": [TaskRes],
 *   "cc": [CcRes], "my_task": TaskRes | null, "auto_passed_nodes": 0 }
 * ```
 * 注意是**嵌套**结构，不是架构文档 §10.4.1 那个扁平样例（文档样例已过时）。
 *
 * # 进度条位置怎么算
 *
 * 纯读后端：`nodes[].status` ∈ `PENDING|ACTIVE|APPROVED|REJECTED|SKIPPED|TERMINATED`，
 * 前端只把它们画出来（`FlowProgress`）。**不自己推「走到第几步」**——
 * 引擎写什么就是什么，猜一次就可能和真实状态不一致。
 *
 * # 操作区可见性
 *
 * 只有后端下发 `my_task`（我在本单上的 PENDING 待办）时才出现；
 * 发起人且在跑 → 显示「撤回」（行级判定仍在后端）。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';
import {
  addSignFlowTask,
  approveFlowTask,
  getFlowInstance,
  getFlowVersion,
  listFlowVersions,
  pageFlowDefs,
  rejectFlowTask,
  transferFlowTask,
  withdrawFlowInstance,
  type FlowEdge,
  type FlowInstanceDetail,
  type FlowNode,
  type FlowNodeProgress,
  type FlowTaskItem,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import { nodeTypeLabel, topoOrder } from '@/views/business/flow/graph';
import type { FlowProgressNode } from '@/views/business/flow/components/FlowProgress.vue';
import StatusBadge from '@/views/business/task/components/StatusBadge.vue';
import UserSelect from '@/views/business/task/components/UserSelect.vue';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import { useBadgeStore } from '@/stores/badge';

const route = useRoute();
const router = useRouter();
const userStore = useUserStore();

/** 实例 id */
const instanceId = computed(() => String(route.params.id ?? ''));

/** 详情 */
const detail = ref<FlowInstanceDetail | null>(null);
/** 加载中 */
const loading = ref(false);
const badgeStore = useBadgeStore();
/** 加载失败提示 */
const loadError = ref('');

/** 轮询句柄（RUNNING 才轮询，30s 一次；PRD C-16：不做 WebSocket） */
let pollTimer: number | null = null;

/** 实例绑定的那张图（拉回来只为「未到达节点也要显示成灰色」+ 拓扑排序） */
const graphNodes = ref<FlowNode[]>([]);
const graphEdges = ref<FlowEdge[]>([]);

/**
 * 按 `flow_code` + `version_no` 找回实例绑定的版本整图。
 *
 * 三步（都走真接口，失败就静默退化，页面不空）：
 * 1. `GET /def/page?keyword=<flow_code>` → 找到 def id；
 * 2. `GET /def/{id}/versions` → 找 `version_no` 相同的那一版；
 * 3. `GET /version/{vid}` → nodes / edges。
 */
async function loadGraph(flowCode: string, versionNo: number): Promise<void> {
  if (!flowCode || !versionNo) {
    return;
  }
  try {
    const page = await pageFlowDefs({ keyword: flowCode, page_size: 20 });
    const def = (page.list ?? []).find((item) => item.flow_code === flowCode);
    if (!def) {
      return;
    }
    const versions = await listFlowVersions(def.id);
    const version = (versions.list ?? []).find((item) => item.version_no === versionNo);
    if (!version) {
      return;
    }
    const detailVersion = await getFlowVersion(version.id);
    graphNodes.value = detailVersion.nodes ?? [];
    graphEdges.value = detailVersion.edges ?? [];
  } catch {
    // 图拉不到不影响主流程：进度条会退化到「只用后端节点」
    graphNodes.value = [];
    graphEdges.value = [];
  }
}

/**
 * 进度条要的形状。
 *
 * # 为什么要拉一次流程定义
 *
 * 后端的 `nodes[]` **只包含「已到达过」的节点行**（引擎在激活 / 跳过时才建行），
 * 未到达的节点根本没有行；而且它的排序是 `round, created_at, node_key`，
 * 同一秒创建时按 key 排 → 会出现「审批节点排在开始节点前面」。
 *
 * 卡面要求进度条能画出**未到的灰色**，所以这里按实例绑定的版本
 * （`flow_code` + `version_no`）把整图拉回来，用 `topoOrder` 排好，
 * 再与后端状态合并：图上有、后端没有 → `PENDING`（未到）。
 * 图拉不到（权限 / 已删）时退化为「只用后端节点、按 START 优先排序」，页面不空。
 */
const progressNodes = computed<FlowProgressNode[]>(() => {
  const rows = detail.value?.nodes ?? [];
  // 同一节点多轮（退回）时取最新一轮
  const latest = new Map<string, FlowNodeProgress>();
  for (const row of rows) {
    const current = latest.get(row.node_key);
    if (!current || row.round >= current.round) {
      latest.set(row.node_key, row);
    }
  }
  const order = topoOrder(graphNodes.value, graphEdges.value);
  if (order.length === 0) {
    // 图没拉到：按 START 优先 + 名字排序兜底
    return [...latest.values()]
      .sort((a, b) => {
        if (a.node_type === 'START') {
          return -1;
        }
        if (b.node_type === 'START') {
          return 1;
        }
        return a.node_key.localeCompare(b.node_key);
      })
      .map((row) => toProgressNode(row));
  }
  return order.map((key) => {
    const row = latest.get(key);
    if (row) {
      return toProgressNode(row);
    }
    const node = graphNodes.value.find((item) => item.key === key);
    return {
      node_key: key,
      name: node?.name ?? key,
      type: node?.type ?? 'APPROVAL',
      round: 1,
      status: 'PENDING',
      approver_count: 0,
      approved_count: 0,
      duration_ms: 0,
    };
  });
});

/** 后端节点行 → 进度条节点 */
function toProgressNode(row: FlowNodeProgress): FlowProgressNode {
  return {
    node_key: row.node_key,
    name: row.node_name,
    type: row.node_type,
    round: row.round,
    status: row.status,
    approver_count: row.approver_count,
    approved_count: row.approved_count,
    duration_ms: row.duration_ms ?? 0,
  };
}

/**
 * 表单字段（读 `detail.instance.form_data`，D4a 起接口下发）。
 *
 * 口径照卡面：键名原样展示（不做中文映射 —— 键里是什么就显示什么），
 * 值用 `String(v)`；`null` / `undefined` 显示空；数组 / 对象用 JSON 字符串。
 */
function formatFormValue(value: unknown): string {
  if (value === null || value === undefined) {
    return '';
  }
  if (typeof value === 'object') {
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  }
  return String(value);
}

/** 附带的表单字段（无表单 → 空数组，模板显示「发起时未填写表单内容」） */
const formFields = computed<Array<{ key: string; value: string }>>(() => {
  const data = detail.value?.instance?.form_data;
  if (!data || typeof data !== 'object') {
    return [];
  }
  return Object.entries(data).map(([key, value]) => ({ key, value: formatFormValue(value) }));
});

/** 时间线条目 */
interface TimelineItem {
  /** 时间 */
  at: string;
  /** 谁 */
  who: string;
  /** 什么操作 */
  what: string;
  /** 意见 */
  comment?: string;
  /** 是否系统代点 */
  isAuto: boolean;
  /** 是否自审 */
  isSelf: boolean;
  /** 圆点样式 */
  cls: string;
}

/** 时间线（后端 `timeline` 是 TaskRes 数组，含 SKIPPED / TRANSFERRED 留痕） */
const timelineItems = computed<TimelineItem[]>(() =>
  (detail.value?.timeline ?? []).map((task: FlowTaskItem) => ({
    at: task.completed_at ?? task.created_at,
    who: task.assignee_name || '系统',
    what: `${taskStatusText(task.status)}（${task.node_key} 第 ${task.round} 轮）`,
    comment: task.comment || undefined,
    isAuto: task.is_auto,
    isSelf: task.is_self_approval,
    cls: task.status === 'APPROVED' ? 'is-done' : task.status === 'REJECTED' ? 'is-rejected' : 'is-pending',
  })),
);

/** 我是否可以撤回（发起人 + 在跑 + 后端下发 my_task 为空） */
const canWithdraw = computed(() => {
  const instance = detail.value?.instance;
  if (!instance) {
    return false;
  }
  const me = userStore.userInfo?.id ?? '';
  return (
    !!me &&
    instance.initiator_id === me &&
    (instance.status === 'RUNNING' || instance.status === 'PENDING') &&
    !detail.value?.my_task
  );
});

/** 时间格式化 */
function formatTime(value: string): string {
  if (!value) {
    return '—';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  const pad = (input: number): string => String(input).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 耗时文案 */
function durationText(ms: number): string {
  if (!ms || ms < 0) {
    return '—';
  }
  if (ms < 60000) {
    return `${Math.max(1, Math.round(ms / 1000))} 秒`;
  }
  if (ms < 3600000) {
    return `${Math.round(ms / 60000)} 分钟`;
  }
  if (ms < 86400000) {
    return `${(ms / 3600000).toFixed(1)} 小时`;
  }
  return `${(ms / 86400000).toFixed(1)} 天`;
}

/** 节点类型中文名 */
function nodeTypeText(type: string): string {
  return nodeTypeLabel(type);
}

/** 任务状态中文名 */
function taskStatusText(status: string): string {
  const map: Record<string, string> = {
    APPROVED: '同意',
    REJECTED: '驳回',
    SKIPPED: '跳过',
    TERMINATED: '随流程终止',
    TRANSFERRED: '转办',
    DELEGATED: '委托',
    PENDING: '待处理',
  };
  return map[status] ?? status;
}

/** 载入详情 */
async function loadDetail(silent = false): Promise<void> {
  if (!instanceId.value) {
    return;
  }
  if (!silent) {
    loading.value = true;
  }
  try {
    const data = await getFlowInstance(instanceId.value);
    detail.value = data;
    loadError.value = '';
    if (graphNodes.value.length === 0) {
      // 只在首次（或图还没拉到）时拉图：轮询不必重复拉
      await loadGraph(data.instance.flow_code, data.instance.version_no);
    }
  } catch (error) {
    // 静默轮询失败不打扰用户，首屏失败才提示
    if (!silent) {
      detail.value = null;
      loadError.value = error instanceof ApiError ? error.message : '审批单加载失败';
    }
  } finally {
    loading.value = false;
  }
  schedulePoll();
}

/** 运行中才轮询 */
function schedulePoll(): void {
  if (pollTimer) {
    window.clearTimeout(pollTimer);
    pollTimer = null;
  }
  const status = detail.value?.instance.status;
  if (status === 'RUNNING' || status === 'PENDING') {
    pollTimer = window.setTimeout(() => {
      void loadDetail(true);
    }, 30000);
  }
}

// ==================== 我的操作 ====================

/** 同意中 */
const approving = ref(false);
/** 审批意见（同意时随请求提交；空则视为不填） */
const approveComment = ref('');

/** 同意 */
async function onApprove(): Promise<void> {
  const taskId = detail.value?.my_task?.id;
  if (!taskId) {
    return;
  }
  approving.value = true;
  try {
    const comment = approveComment.value.trim();
    const result = await approveFlowTask(taskId, comment ? { comment } : {});
void badgeStore.refresh();
    approveComment.value = '';
    MessagePlugin.success(result.instance_status === 'APPROVED' ? '已同意，审批单已通过' : '已同意');
    await loadDetail();
  } catch (error) {
    if (error instanceof ApiError && error.code === 65020) {
      MessagePlugin.warning('该待办已被处理，已刷新');
      await loadDetail();
    } else {
      MessagePlugin.error(error instanceof ApiError ? error.message : '操作失败');
    }
  } finally {
    approving.value = false;
  }
}

/** 驳回 */
const rejectVisible = ref(false);
const rejecting = ref(false);
const rejectForm = ref({ returnTo: 'PREV', comment: '' });

/** 退回方式 */
const returnOptions = [
  { label: '退回上一节点', value: 'PREV' },
  { label: '退回发起人', value: 'INITIATOR' },
];

/** 打开驳回 */
function openReject(): void {
  rejectForm.value = { returnTo: 'PREV', comment: '' };
  rejectVisible.value = true;
}

/** 提交驳回（意见 ≥5 字） */
async function submitReject(): Promise<void> {
  const taskId = detail.value?.my_task?.id;
  if (!taskId) {
    return;
  }
  const comment = rejectForm.value.comment.trim();
  if (comment.length < 5) {
    MessagePlugin.warning('驳回意见至少 5 个字');
    return;
  }
  rejecting.value = true;
  try {
    await rejectFlowTask(taskId, { comment, return_to: rejectForm.value.returnTo });
void badgeStore.refresh();
    rejectVisible.value = false;
    MessagePlugin.success('已驳回');
    await loadDetail();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '驳回失败');
  } finally {
    rejecting.value = false;
  }
}

/** 转办 */
const transferVisible = ref(false);
const transferring = ref(false);
const transferForm = ref({ toUserId: null as string | null, reason: '' });

/** 打开转办 */
function openTransfer(): void {
  transferForm.value = { toUserId: null, reason: '' };
  transferVisible.value = true;
}

/** 提交转办 */
async function submitTransfer(): Promise<void> {
  const taskId = detail.value?.my_task?.id;
  if (!taskId) {
    return;
  }
  if (!transferForm.value.toUserId) {
    MessagePlugin.warning('请选择转办对象');
    return;
  }
  transferring.value = true;
  try {
    await transferFlowTask(taskId, {
      to_user_id: transferForm.value.toUserId,
      reason: transferForm.value.reason.trim() || undefined,
    });
    transferVisible.value = false;
    MessagePlugin.success('已转办');
    await loadDetail();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '转办失败');
  } finally {
    transferring.value = false;
  }
}

/** 加签 */
const addSignVisible = ref(false);
const addingSign = ref(false);
const addSignForm = ref({ userId: null as string | null });

/** 打开加签 */
function openAddSign(): void {
  addSignForm.value = { userId: null };
  addSignVisible.value = true;
}

/** 提交加签 */
async function submitAddSign(): Promise<void> {
  const taskId = detail.value?.my_task?.id;
  if (!taskId) {
    return;
  }
  if (!addSignForm.value.userId) {
    MessagePlugin.warning('请选择加签人');
    return;
  }
  addingSign.value = true;
  try {
    await addSignFlowTask(taskId, { user_ids: [addSignForm.value.userId] });
    addSignVisible.value = false;
    MessagePlugin.success('已加签');
    await loadDetail();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '加签失败');
  } finally {
    addingSign.value = false;
  }
}

/** 撤回中 */
const withdrawing = ref(false);

/** 撤回（只有零人已审才允许） */
async function onWithdraw(): Promise<void> {
  withdrawing.value = true;
  try {
    await withdrawFlowInstance(instanceId.value);
    MessagePlugin.success('已撤回');
    await loadDetail();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '撤回失败');
  } finally {
    withdrawing.value = false;
  }
}

onMounted(() => {
  void loadDetail();
});

onBeforeUnmount(() => {
  if (pollTimer) {
    window.clearTimeout(pollTimer);
    pollTimer = null;
  }
});
</script>

<style scoped lang="less">
.flow-detail-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.back {
  align-self: flex-start;
}

.card {
  border-radius: 8px;
}

.detail-loading,
.detail-empty {
  display: flex;
  flex-direction: column;
  gap: 8px;
  align-items: center;
  justify-content: center;
  padding: 60px 0;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
}

.head {
  display: flex;
  gap: 12px;
  align-items: flex-start;
  justify-content: space-between;

  &__main {
    min-width: 0;
  }

  &__title-row {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
  }

  &__title {
    font-size: 17px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__meta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
    margin-top: 6px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__no {
    font-family: var(--app-font-mono, monospace);
    color: var(--td-text-color-secondary);
  }

  &__sep {
    margin: 0 4px;
  }

  &__version {
    color: var(--td-text-color-secondary);
  }

  &__actions {
    display: flex;
    flex: none;
    gap: 8px;
  }
}

.progress-wrap {
  padding: 8px 0 0;
  margin-top: 12px;
  border-top: 1px solid var(--td-component-stroke);
}

.cc-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  align-items: center;
  margin-top: 8px;
  font-size: 12px;

  &__label {
    color: var(--td-text-color-placeholder);
  }

  &__empty {
    color: var(--td-text-color-disabled);
  }
}

.section-title {
  margin-bottom: 12px;
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.content {
  display: flex;
  flex-direction: column;
  gap: 10px;

  &__title {
    margin: 0;
    font-size: 15px;
    font-weight: 500;
    color: var(--td-text-color-primary);
  }

  &__comment {
    margin: 0;
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }

  &__text {
    margin: 0;
    font-size: 14px;
    line-height: 1.8;
    color: var(--td-text-color-primary);
    white-space: pre-wrap;
    word-break: break-word;
  }

  &__empty {
    margin: 0;
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }

  &__fields {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 10px;
    margin-top: 10px;
    border-top: 1px dashed var(--td-component-stroke);
  }

  &__field {
    display: flex;
    gap: 10px;
    font-size: 13px;
  }

  &__field-key {
    min-width: 110px;
    color: var(--td-text-color-placeholder);
  }

  &__field-value {
    color: var(--td-text-color-primary);
    word-break: break-word;
  }
}

.timeline-card {
  margin-top: 12px;
}

.timeline {
  padding: 0;
  margin: 0;
  list-style: none;

  &__item {
    position: relative;
    display: flex;
    gap: 10px;
    padding-bottom: 14px;

    &:not(:last-child)::before {
      position: absolute;
      top: 14px;
      bottom: 0;
      left: 4px;
      content: '';
      border-left: 1px dashed var(--td-component-stroke);
    }
  }

  &__dot {
    z-index: 1;
    width: 9px;
    height: 9px;
    flex: none;
    margin-top: 5px;
    background: var(--td-text-color-placeholder);
    border-radius: 50%;

    &.is-done {
      background: var(--td-success-color);
    }

    &.is-rejected {
      background: var(--td-error-color);
    }

    &.is-pending {
      background: var(--td-text-color-disabled);
    }
  }

  &__body {
    min-width: 0;
  }

  &__head {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: baseline;
    font-size: 13px;
  }

  &__who {
    font-weight: 500;
    color: var(--td-text-color-primary);
  }

  &__what {
    color: var(--td-text-color-secondary);
  }

  &__time {
    margin-top: 2px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__comment {
    padding: 6px 10px;
    margin-top: 6px;
    font-size: 13px;
    color: var(--td-text-color-primary);
    background: var(--td-bg-color-secondarycontainer);
    border-radius: 4px;
  }

  &-empty {
    padding: 20px 0;
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }
}

.node-list {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.node-item {
  padding: 8px 10px;
  background: var(--td-bg-color-secondarycontainer);
  border-radius: 6px;

  &__head {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
  }

  &__name {
    font-size: 13px;
    font-weight: 500;
    color: var(--td-text-color-primary);
  }

  &__meta {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    margin-top: 4px;
    font-size: 11px;
    color: var(--td-text-color-placeholder);
  }
}

.action-card {
  margin-top: 12px;
}

.action-tip {
  margin: 8px 0 0;
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-text-color-placeholder);
}
</style>
