<template>
  <div class="flow-task-page">
    <t-card :bordered="false" class="card">
      <div class="task-head">
        <!-- TDesign 的 t-tabs 用 <t-tab-panel> 子节点，不是 items 属性（items 会被当成普通 attr 渲染成空壳） -->
        <t-tabs v-model="activeTab" @change="onTabChange">
          <t-tab-panel value="todo" label="待我审批" />
          <t-tab-panel value="done" label="我已审批" />
          <t-tab-panel value="mine" label="我发起的" />
          <t-tab-panel value="cc" label="抄送我的" />
        </t-tabs>
        <t-button variant="outline" size="small" :loading="loading" @click="loadCurrent">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>
      </div>

      <t-alert v-if="tabError" class="task-alert" theme="error" :message="tabError">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadCurrent">重试</t-button>
        </template>
      </t-alert>

      <div v-if="loading" class="task-state">加载中…</div>

      <div v-else-if="cards.length === 0" class="task-empty">
        <t-icon name="inbox" size="28px" />
        <p>{{ emptyText }}</p>
      </div>

      <div v-else class="task-list">
        <div v-for="card in cards" :key="card.id" class="task-card" @click="openDetail(card)">
          <div class="task-card__main">
            <div class="task-card__title">
              <span class="task-card__title-text">{{ card.title || '（无标题）' }}</span>
              <span v-if="card.flowName" class="task-card__flow">{{ card.flowName }}</span>
              <t-tag v-if="card.tag" size="small" variant="light" :theme="card.tagTheme">{{ card.tag }}</t-tag>
            </div>
            <div class="task-card__meta">
              <span class="task-card__no">{{ card.instanceNo || '—' }}</span>
              <span class="task-card__dot">·</span>
              <span>发起人：{{ card.initiatorName || '—' }}</span>
              <template v-if="card.nodeLabel">
                <span class="task-card__dot">·</span>
                <span>当前节点：{{ card.nodeLabel }}</span>
              </template>
              <span v-if="card.extra" class="task-card__dot">·</span>
              <span v-if="card.extra">{{ card.extra }}</span>
            </div>
          </div>
          <div class="task-card__side">
            <span class="task-card__time">{{ formatTime(card.at) }}</span>
            <span v-if="card.waitedMs !== undefined" class="task-card__waited">{{ waitedText(card.waitedMs) }}</span>
            <StatusBadge v-if="card.status" :domain="card.statusDomain" :status="card.status" />
          </div>
          <div v-if="activeTab === 'todo'" class="task-card__actions" @click.stop>
            <t-button size="small" theme="primary" :loading="actingId === card.id" @click="onApprove(card)">同意</t-button>
            <t-button size="small" theme="danger" variant="outline" @click="openReject(card)">驳回</t-button>
            <t-button size="small" variant="outline" @click="openTransfer(card)">转办</t-button>
            <t-button size="small" variant="outline" @click="openAddSign(card)">加签</t-button>
          </div>
        </div>
      </div>
    </t-card>

    <!-- 驳回抽屉：退回方式 + 意见（≥5 字） -->
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
          <t-textarea
            v-model="rejectForm.comment"
            placeholder="说明驳回原因，便于发起人修改后重新提交"
            :autosize="{ minRows: 4, maxRows: 8 }"
            :maxlength="500"
          />
          <p class="form-tip">{{ rejectForm.comment.trim().length }}/5 字</p>
        </t-form-item>
      </t-form>
    </t-drawer>

    <!-- 转办抽屉 -->
    <t-drawer
      v-model:visible="transferVisible"
      header="转办"
      size="400px"
      :confirm-btn="{ content: '确认转办', theme: 'primary', loading: transferring }"
      @confirm="submitTransfer"
    >
      <t-form label-align="top">
        <t-form-item label="转办给" required>
          <UserSelect v-model="transferForm.toUserId" placeholder="搜索用户名 / 姓名" />
        </t-form-item>
        <t-form-item label="说明">
          <t-textarea v-model="transferForm.reason" placeholder="为什么转办（可选）" :autosize="{ minRows: 3, maxRows: 6 }" />
        </t-form-item>
      </t-form>
    </t-drawer>

    <!-- 加签抽屉 -->
    <t-drawer
      v-model:visible="addSignVisible"
      header="加签"
      size="400px"
      :confirm-btn="{ content: '确认加签', theme: 'primary', loading: addingSign }"
      @confirm="submitAddSign"
    >
      <t-form label-align="top">
        <t-form-item label="加签人" required>
          <UserSelect v-model="addSignForm.userId" placeholder="搜索用户名 / 姓名" />
        </t-form-item>
        <p class="form-tip">一次加签一人（后端接口收 `user_ids[]`，这里先只做单选）</p>
      </t-form>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 审批中心（卡面页面 4 / `/flow/task`）：待办 / 已办 / 我发起 / 抄送我的 四个 Tab。
 *
 * # 数据来源（全部真实接口，D4a 后无前端补字段）
 *
 * | Tab | 接口 | 出参 |
 * |---|---|---|
 * | 待我审批 | `GET /business/flow/task/todo` | `PageData<TaskRes>` |
 * | 我已审批 | `GET /business/flow/task/done` | `PageData<TaskRes>` |
 * | 我发起的 | `GET /business/flow/instance/my`（`initiator_id = 我`，按发起时间倒序） | `PageData<InstanceRes>` |
 * | 抄送我的 | `GET /business/flow/task/cc` | `PageData<CcRes>` |
 *
 * `TaskRes` / `CcRes` 自 D4a 起下发 `flow_name` / `initiator_name` / `current_node_name` /
 * `arrived_at`，所以卡片上的流程名、发起人、当前节点中文名全部直接取字段，
 * 「当前节点」不再显示 `node_key`，时间口径统一用 `arrived_at`。
 */
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';
import {
  addSignFlowTask,
  approveFlowTask,
  listDoneTasks,
  listMyCc,
  listMyInstances,
  listTodoTasks,
  rejectFlowTask,
  transferFlowTask,
  type FlowTaskItem,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import type { StatusDomain } from '@/constants/status-map';
import StatusBadge from '@/views/business/task/components/StatusBadge.vue';
import UserSelect from '@/views/business/task/components/UserSelect.vue';

const router = useRouter();

/** 统一卡片形状（四类列表字段各不相同，这里归一成「首页要展示什么」） */
interface CardItem {
  /** 记录 id（待办 id / 抄送 id / 实例 id） */
  id: string;
  /** 实例 id（点开详情用） */
  instanceId: string;
  /** 待办 id（同意 / 驳回 / 转办 / 加签用） */
  taskId?: string;
  /** 标题 */
  title: string;
  /** 单据号 */
  instanceNo: string;
  /** 流程名 */
  flowName: string;
  /** 发起人姓名（空串 = 未知 / 已删） */
  initiatorName: string;
  /** 当前节点的显示名（中文名；缺失时回退 `—`） */
  nodeLabel: string;
  /** 附注（如「第 2 轮」「加签」） */
  extra: string;
  /** 时间（到达时间 `arrived_at` / 发起时间） */
  at: string;
  /** 已等待毫秒（前端按到达时间算） */
  waitedMs?: number;
  /** 状态字面量 */
  status: string;
  /** 状态域 */
  statusDomain: StatusDomain;
  /** 小标签文案（可无） */
  tag?: string;
  /** 小标签主题（有 tag 时必填） */
  tagTheme?: 'default' | 'primary' | 'success' | 'warning' | 'danger';
}

/** 当前 Tab（`t-tab-panel` 的 value） */
const activeTab = ref('todo');
/** 卡片 */
const cards = ref<CardItem[]>([]);
/** 加载中 */
const loading = ref(false);
/** 当前 Tab 的加载错误 */
const tabError = ref('');
/** 正在提交的卡片 id（防重复点击） */
const actingId = ref('');

/** 空态文案（按 Tab 区分，卡面要求 4 种空态） */
const emptyText = computed(() => {
  const map: Record<string, string> = {
    todo: '没有待你审批的单子',
    done: '你还没有审批过任何单子',
    mine: '你还没有发起过审批（去「发起审批」建一张）',
    cc: '没有抄送给你的单子',
  };
  return map[activeTab.value] ?? '暂无数据';
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
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 「已等 X」文案 */
function waitedText(ms: number): string {
  if (!ms || ms <= 0) {
    return '已等 1 分钟';
  }
  const hours = Math.floor(ms / 3600000);
  if (hours < 1) {
    return `已等 ${Math.max(1, Math.floor(ms / 60000))} 分钟`;
  }
  if (hours < 24) {
    return `已等 ${hours} 小时`;
  }
  return `已等 ${Math.floor(hours / 24)} 天`;
}

/** 待办 / 已办卡片（`TaskRes` → 卡片；字段全部来自后端，不做前端补全） */
function taskToCard(task: FlowTaskItem): CardItem {
  const tags: string[] = [];
  if (task.is_added_sign) {
    tags.push('加签');
  }
  if (task.task_kind === 'RESUBMIT') {
    tags.push('重新提交');
  }
  return {
    id: task.id,
    instanceId: task.instance_id,
    taskId: task.id,
    title: task.title,
    instanceNo: task.instance_no,
    flowName: task.flow_name,
    initiatorName: task.initiator_name,
    // 节点显示中文名（`current_node_name`）；节点行缺失时后端给空串，模板回退 `—`
    nodeLabel: task.current_node_name,
    extra: task.round > 1 ? `第 ${task.round} 轮` : '',
    at: task.arrived_at,
    waitedMs: task.task_kind === 'APPROVAL' ? Math.max(0, Date.now() - new Date(task.arrived_at).getTime()) : undefined,
    status: task.status,
    statusDomain: 'flow-task',
    tag: tags[0],
    tagTheme: task.task_kind === 'RESUBMIT' ? 'warning' : 'default',
  };
}

/** 载入当前 Tab */
async function loadCurrent(): Promise<void> {
  loading.value = true;
  tabError.value = '';
  cards.value = [];
  try {
    if (activeTab.value === 'todo') {
      const data = await listTodoTasks({ page: 1, page_size: 50 });
      cards.value = (data.list ?? []).map(taskToCard);
    } else if (activeTab.value === 'done') {
      const data = await listDoneTasks({ page: 1, page_size: 50 });
      cards.value = (data.list ?? []).map(taskToCard);
    } else if (activeTab.value === 'mine') {
      // 服务端按 `initiator_id = 我` 过滤并倒序，前端不再过滤 / 不再拼数据
      const data = await listMyInstances({ page: 1, page_size: 50 });
      cards.value = (data.list ?? []).map(
        (item): CardItem => ({
          id: item.id,
          instanceId: item.id,
          title: item.title,
          instanceNo: item.instance_no,
          flowName: `${item.flow_name}（v${item.version_no}）`,
          initiatorName: item.initiator_name,
          // InstanceRes 没有 `current_node_name`，这里不编造节点名，只把「轮到我处理」放进附注
          nodeLabel: '',
          extra: [
            item.my_task_id ? '待我处理' : '',
            item.auto_passed_nodes > 0 ? `自动通过 ${item.auto_passed_nodes} 个节点` : '',
          ]
            .filter(Boolean)
            .join(' · '),
          at: item.started_at,
          status: item.status,
          statusDomain: 'flow-instance',
        }),
      );
    } else {
      const data = await listMyCc({ page: 1, page_size: 50 });
      cards.value = (data.list ?? []).map((item) => ({
        id: item.id,
        instanceId: item.instance_id,
        title: item.title,
        instanceNo: item.instance_no,
        flowName: item.flow_name,
        initiatorName: item.initiator_name,
        nodeLabel: item.current_node_name,
        extra: item.round > 1 ? `第 ${item.round} 轮` : '',
        at: item.arrived_at,
        status: item.is_read ? 'READ' : 'UNREAD',
        statusDomain: 'flow-cc' as StatusDomain,
        tag: item.is_read ? undefined : '未读',
        tagTheme: 'primary' as const,
      }));
    }
  } catch (error) {
    cards.value = [];
    tabError.value = `${error instanceof ApiError ? error.message : '接口异常'}（下面为空态，可点「重试」）`;
  } finally {
    loading.value = false;
  }
}

/** 切 Tab */
function onTabChange(): void {
  void loadCurrent();
}

/** 打开详情 */
function openDetail(card: CardItem): void {
  void router.push(`/flow/instance/${card.instanceId}`);
}

// ==================== 同意 ====================

/** 同意（会签时只记自己那一票） */
async function onApprove(card: CardItem): Promise<void> {
  if (!card.taskId) {
    return;
  }
  actingId.value = card.id;
  try {
    const result = await approveFlowTask(card.taskId);
    MessagePlugin.success(result.instance_status === 'APPROVED' ? '已同意，审批单已通过' : '已同意');
    await loadCurrent();
  } catch (error) {
    // 65020 = 已被别人处理：按「已处理，刷新」展示，不当成错误弹窗
    if (error instanceof ApiError && error.code === 65020) {
      MessagePlugin.warning('该待办已被处理，已为你刷新');
      await loadCurrent();
    } else {
      MessagePlugin.error(error instanceof ApiError ? error.message : '同意失败');
    }
  } finally {
    actingId.value = '';
  }
}

// ==================== 驳回 ====================

/** 驳回抽屉可见 */
const rejectVisible = ref(false);
/** 驳回中 */
const rejecting = ref(false);
/** 当前操作卡片 */
const currentCard = ref<CardItem | null>(null);
/** 驳回表单 */
const rejectForm = ref({ returnTo: 'PREV', comment: '' });

/** 退回方式选项（`NONE` = 驳回终止，仅管理员，这里不放出来避免误用） */
const returnOptions = [
  { label: '退回上一节点', value: 'PREV' },
  { label: '退回发起人', value: 'INITIATOR' },
];

/** 打开驳回抽屉 */
function openReject(card: CardItem): void {
  currentCard.value = card;
  rejectForm.value = { returnTo: 'PREV', comment: '' };
  rejectVisible.value = true;
}

/** 提交驳回（意见 ≥5 字，前端先拦一次） */
async function submitReject(): Promise<void> {
  const card = currentCard.value;
  if (!card?.taskId) {
    return;
  }
  const comment = rejectForm.value.comment.trim();
  if (comment.length < 5) {
    MessagePlugin.warning('驳回意见至少 5 个字');
    return;
  }
  rejecting.value = true;
  try {
    await rejectFlowTask(card.taskId, { comment, return_to: rejectForm.value.returnTo });
    rejectVisible.value = false;
    MessagePlugin.success('已驳回');
    await loadCurrent();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '驳回失败');
  } finally {
    rejecting.value = false;
  }
}

// ==================== 转办 ====================

const transferVisible = ref(false);
const transferring = ref(false);
const transferForm = ref({ toUserId: null as string | null, reason: '' });

/** 打开转办 */
function openTransfer(card: CardItem): void {
  currentCard.value = card;
  transferForm.value = { toUserId: null, reason: '' };
  transferVisible.value = true;
}

/** 提交转办 */
async function submitTransfer(): Promise<void> {
  const card = currentCard.value;
  if (!card?.taskId) {
    return;
  }
  if (!transferForm.value.toUserId) {
    MessagePlugin.warning('请选择转办对象');
    return;
  }
  transferring.value = true;
  try {
    await transferFlowTask(card.taskId, {
      to_user_id: transferForm.value.toUserId,
      reason: transferForm.value.reason.trim() || undefined,
    });
    transferVisible.value = false;
    MessagePlugin.success('已转办');
    await loadCurrent();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '转办失败');
  } finally {
    transferring.value = false;
  }
}

// ==================== 加签 ====================

const addSignVisible = ref(false);
const addingSign = ref(false);
const addSignForm = ref({ userId: null as string | null });

/** 打开加签 */
function openAddSign(card: CardItem): void {
  currentCard.value = card;
  addSignForm.value = { userId: null };
  addSignVisible.value = true;
}

/** 提交加签 */
async function submitAddSign(): Promise<void> {
  const card = currentCard.value;
  if (!card?.taskId) {
    return;
  }
  if (!addSignForm.value.userId) {
    MessagePlugin.warning('请选择加签人');
    return;
  }
  addingSign.value = true;
  try {
    await addSignFlowTask(card.taskId, { user_ids: [addSignForm.value.userId] });
    addSignVisible.value = false;
    MessagePlugin.success('已加签');
    await loadCurrent();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '加签失败');
  } finally {
    addingSign.value = false;
  }
}

onMounted(() => {
  void loadCurrent();
});
</script>

<style scoped lang="less">
.flow-task-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.card {
  border-radius: 8px;
}

.task-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
}

.task-alert {
  margin-bottom: 12px;
}

.task-state,
.task-empty {
  display: flex;
  flex-direction: column;
  gap: 8px;
  align-items: center;
  justify-content: center;
  padding: 48px 0;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
}

.task-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.task-card {
  display: flex;
  gap: 12px;
  align-items: center;
  padding: 12px 14px;
  cursor: pointer;
  background: var(--td-bg-color-container);
  border: 1px solid var(--td-component-stroke);
  border-radius: 8px;
  transition: border-color 0.15s ease, background 0.15s ease;

  &:hover {
    background: var(--td-bg-color-container-hover);
    border-color: var(--td-brand-color-4);
  }

  &__main {
    flex: 1;
    min-width: 0;
  }

  &__title {
    display: flex;
    gap: 8px;
    align-items: center;
    overflow: hidden;
    font-size: 14px;
    font-weight: 500;
    color: var(--td-text-color-primary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__title-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* 流程名（`flow_name`）：标题旁的中性小 chip；中性色是为跨 22 套配色都能过 AA 对比 */
  &__flow {
    flex: none;
    padding: 0 6px;
    font-size: 12px;
    line-height: 17px;
    color: var(--td-text-color-secondary);
    white-space: nowrap;
    background: var(--td-bg-color-secondarycontainer);
    border: 1px solid var(--td-component-stroke);
    border-radius: 3px;
  }

  &__meta {
    margin-top: 4px;
    overflow: hidden;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__no {
    font-family: var(--app-font-mono, monospace);
    color: var(--td-text-color-secondary);
  }

  &__dot {
    margin: 0 6px;
  }

  &__side {
    display: flex;
    flex: none;
    flex-direction: column;
    gap: 3px;
    align-items: flex-end;
    font-size: 12px;
  }

  &__time {
    color: var(--td-text-color-secondary);
  }

  &__waited {
    color: var(--td-warning-color);
  }

  &__actions {
    display: flex;
    flex: none;
    gap: 6px;
  }
}

.form-tip {
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}
</style>
