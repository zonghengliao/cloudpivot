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

      <!-- P11 前端防御：实例已结束（实例 CANCELLED/APPROVED/REJECTED/WITHDRAWN）但待办还是
           PENDING 的孤儿行已被过滤掉 —— 后端已修根因与存量数据，这里挡住「万一还有」的历史脏数据，
           不让用户看到一个点不动的按钮。 -->
      <t-alert
        v-if="hiddenEndedCount > 0"
        class="task-alert"
        theme="warning"
        data-testid="flow-todo-hidden-ended"
        :message="`已隐藏 ${hiddenEndedCount} 条「审批单已结束」的待办：这些单子已被取消 / 通过 / 驳回，不需要再处理`"
      />

      <div v-if="loading" class="task-state">加载中…</div>

      <div v-else-if="cards.length === 0" class="task-empty">
        <t-icon name="inbox" size="28px" />
        <p>{{ emptyText }}</p>
      </div>

      <div v-else class="task-list">
        <div v-for="card in cards" :key="card.id" class="task-card" @click="openDetail(card)">
          <div class="task-card__main">
            <div class="task-card__title">
              <!-- 左上角「待处理」状态徽标：只在待办 Tab 出现（状态），与 NEW（新鲜度）区分 -->
              <t-tag v-if="activeTab === 'todo'" class="task-card__todo" theme="warning" size="small">待处理</t-tag>
              <span class="task-card__title-text">{{ card.title || '（无标题）' }}</span>
              <!-- 24 小时内到达 → NEW 角标（仅有待办 Tab 有意义） -->
              <NewDot v-if="card.isNew" variant="badge" title="24 小时内到达的新待办" />
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
            <!-- 摘要（P1-FIX）：不是原始字段，是把 form_data 翻成人话的一行
                 （供应商 / 价税合计 / 数量合计）；拉不到详情就不显示，不编内容 -->
            <div v-if="summaryOf[card.id]" class="task-card__summary" data-testid="flow-task-summary">
              {{ summaryOf[card.id] }}
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
  getFlowInstance,
  listDoneTasks,
  listMyCc,
  listMyInstances,
  listTodoTasks,
  rejectFlowTask,
  transferFlowTask,
  type FlowTaskItem,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import { formSummary, type FormNameLookup } from '@/views/business/flow/form-field-labels';
import {
  emptyNameMaps,
  ensureMasterNameMaps,
  nameLookupOf,
  type MasterNameMaps,
} from '@/views/business/flow/use-flow-master-names';
import NewDot from '@/components/NewDot.vue';
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
  /** 到达时间戳（毫秒；`decorate` 统一算，用于排序） */
  atMs?: number;
  /** 是否为「24 小时内到达」的新内容（仅待办 Tab 标记） */
  isNew?: boolean;
  /** 所属审批实例的状态（P11 防御用；后端老版本可能缺这个字段） */
  instanceStatus?: string;
  /**
   * 实例的表单快照（`form_data`）。
   *
   * - 「我发起的」Tab 直接由 `InstanceRes` 下发，零成本；
   * - 「待我审批」Tab 的 `TaskRes` **没有** `form_data`，由 {@link fillTodoFormData} 逐单补
   *   （上限 {@link SUMMARY_FETCH_LIMIT} 单，够看到首屏；后端给 `TaskRes` 补上该字段后才用收这个补丁）。
   */
  formData?: Record<string, unknown>;
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
/** P11：本次从待办里过滤掉的「实例已结束」条数（>0 时页面顶部给出解释） */
const hiddenEndedCount = ref(0);

/**
 * 审批实例是否已结束（P11 前端防御）。
 *
 * 待办自己还是 `PENDING`、但所属实例已经 `CANCELLED` / `APPROVED` / `REJECTED` / `WITHDRAWN`，
 * 就是一条点不动的孤儿待办（老板 2026-10-31 报的 TSK-202610-000149）。
 * 后端已在「任务取消 / 删除」两条路径上撤审批，并做了存量数据修复；
 * 这里再兜一层：历史脏数据万一还有，也不让用户看到。
 *
 * 字段缺失（后端老版本）时**不隐藏**：看不出来就别误伤真待办。
 * 状态按大写比较，后端返回小写也不会漏判。
 */
function isInstanceEnded(instanceStatus: string | undefined): boolean {
  if (!instanceStatus) {
    return false;
  }
  return ['CANCELLED', 'APPROVED', 'REJECTED', 'WITHDRAWN'].includes(instanceStatus.trim().toUpperCase());
}

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

/** 主数据译名（与详情页共用同一份模块级缓存，这里只是「谁来触发第一次」） */
const masterNames = ref<MasterNameMaps>(emptyNameMaps());
const nameLookup = computed<FormNameLookup>(() => nameLookupOf(masterNames.value));

/** 补译名（失败 = 权限不足 → 摘要里显示 ID 前 8 位，页面不报错） */
async function warmMasterNames(): Promise<void> {
  masterNames.value = await ensureMasterNameMaps();
}

/**
 * 摘要（P1-FIX）：把卡片上的 `form_data` 翻成人话，`卡片 id → 一行摘要`。
 *
 * 为什么是 computed 而不是「加载时算一次字符串」：译名（`supplier_id` → 名称）是**异步**补的，
 * 主数据回来之前只能显示 `(ID: 01a18def…)`。算成 computed 后，主数据一到所有卡片的摘要自动刷新，
 * 不需要重拉列表。
 */
const summaryOf = computed<Record<string, string>>(() => {
  const out: Record<string, string> = {};
  for (const card of cards.value) {
    if (card.formData) {
      out[card.id] = formSummary(card.formData, nameLookup.value);
    }
  }
  return out;
});

/**
 * 待办 Tab 补 `form_data`：`TaskRes` 没有这个字段，逐单读详情。
 *
 * - **上限 {@link SUMMARY_FETCH_LIMIT} 单**：首屏够看，且不把 50 单列表变成 50 个请求；
 * - **不 await**：列表先渲染出来，摘要稍后填进去（不然首屏要多等一轮往返）；
 * - 单条失败（无权限 / 已终止）→ 该卡不显示摘要，不影响别的卡。
 */
const SUMMARY_FETCH_LIMIT = 20;

async function fillTodoFormData(items: CardItem[]): Promise<void> {
  const targets = items.slice(0, SUMMARY_FETCH_LIMIT);
  await Promise.all(
    targets.map(async (item) => {
      try {
        const data = await getFlowInstance(item.instanceId);
        item.formData = data.instance.form_data ?? {};
      } catch {
        // 拉不到就不显示摘要（详情页点进去仍有内容）
      }
    }),
  );
}

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
    instanceStatus: task.instance_status,
    tag: tags[0],
    tagTheme: task.task_kind === 'RESUBMIT' ? 'warning' : 'default',
  };
}

/** 24 小时窗口：`arrived_at` 在窗口内 = 新内容（NEW 角标） */
const NEW_WINDOW_MS = 24 * 60 * 60 * 1000;

/** 是否 24 小时内到达（未来时间 / 非法时间一律不算新） */
function isFresh(at: string): boolean {
  const time = new Date(at).getTime();
  if (!Number.isFinite(time)) {
    return false;
  }
  const age = Date.now() - time;
  return age >= 0 && age <= NEW_WINDOW_MS;
}

/**
 * 卡片统一收尾：算到达时间戳 + 新鲜度，并按到达时间倒序（最新的在上）。
 * 排序放前端做，是因为后端四个接口各自排序口径不同，列表页只需要一个稳定口径。
 */
function decorate(items: CardItem[]): CardItem[] {
  return items
    .map((item) => ({
      ...item,
      atMs: new Date(item.at).getTime() || 0,
      isNew: activeTab.value === 'todo' && isFresh(item.at),
    }))
    .sort((a, b) => (b.atMs ?? 0) - (a.atMs ?? 0));
}

/** 载入当前 Tab */
async function loadCurrent(): Promise<void> {
  loading.value = true;
  tabError.value = '';
  cards.value = [];
  hiddenEndedCount.value = 0;
  try {
    if (activeTab.value === 'todo') {
      const data = await listTodoTasks({ page: 1, page_size: 50 });
      // P11 防御：实例已结束（已取消/已通过/已驳回/已撤回）的孤儿待办不进列表 ——
      // 那是一行点了也没用的数据。后端已修根因 + 存量，这里挡历史脏数据。
      const rows = data.list ?? [];
      const live = rows.filter((item) => !isInstanceEnded(item.instance_status));
      hiddenEndedCount.value = rows.length - live.length;
      cards.value = live.map(taskToCard);
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
          // 「我发起的」的 `InstanceRes` 自带 form_data → 摘要零成本
          formData: item.form_data ?? {},
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
    cards.value = decorate(cards.value);
    // 摘要：列表先出（decorate 之后才补 —— decorate 会重建卡片对象，之前补的 form_data 会丢），
    // 逐单补详情、不 await、上限 20 单；拉不到就不显示摘要行
    if (activeTab.value === 'todo') {
      void fillTodoFormData(cards.value);
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
  // 译名与列表并行拉：摘要要等主数据回来才对（拉不到就显示 ID 前 8 位）
  void warmMasterNames();
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

  /* 「待处理」徽标（warning 浅色 t-tag）不参与收缩，标题先截断 */
  &__todo {
    flex: none;
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

  /* 摘要（P1-FIX）：审批人一眼要看到的那几个业务字段，比 meta 行更亮一档 */
  &__summary {
    margin-top: 4px;
    overflow: hidden;
    font-size: 12px;
    color: var(--td-text-color-secondary);
    text-overflow: ellipsis;
    white-space: nowrap;
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
