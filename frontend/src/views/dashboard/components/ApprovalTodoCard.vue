<template>
  <t-card title="待我审批" :bordered="false" class="apv" data-testid="home-approval-card">
    <template #actions>
      <t-link theme="primary" hover="color" data-testid="home-approval-all" @click="goFlowCenter">
        全部
        <t-icon name="chevron-right" />
      </t-link>
    </template>

    <!-- 首屏骨架：5 行（与工作台其它卡同口径，二次刷新不再闪） -->
    <div v-if="loading && !loaded" class="apv__skeleton">
      <div v-for="row in 5" :key="row" class="apv__skeleton-row" />
    </div>

    <template v-else>
      <ul v-if="rows.length" class="apv__list">
        <li v-for="row in rows" :key="row.id" class="apv__item" :data-testid="`home-approval-item-${row.id}`">
          <span class="apv__dot" />
          <div class="apv__main">
            <!-- 标题区可点：按 biz_type 映射到**业务详情页**（映射表与审批详情页共用一份） -->
            <button type="button" class="apv__title" :title="row.tooltip" @click="openDoc(row)">
              <span class="apv__type">{{ row.typeLabel }}</span>
              <span class="apv__no">{{ row.no }}</span>
            </button>
            <div class="apv__meta">
              <span class="apv__who">{{ row.initiator }}</span>
              <span class="apv__sep">·</span>
              <span class="apv__time">{{ row.relative }}</span>
              <template v-if="row.amount">
                <span class="apv__sep">·</span>
                <span class="apv__amount">{{ row.amount }}</span>
              </template>
            </div>
            <div class="apv__actions">
              <t-button
                size="small"
                variant="outline"
                theme="success"
                :loading="acting.id === row.id && acting.kind === 'approve'"
                :data-testid="`home-approval-approve-${row.id}`"
                @click="openApprove(row)"
              >
                <template #icon><t-icon name="check" /></template>
                同意
              </t-button>
              <t-button
                size="small"
                variant="outline"
                theme="danger"
                :loading="acting.id === row.id && acting.kind === 'reject'"
                :data-testid="`home-approval-reject-${row.id}`"
                @click="openReject(row)"
              >
                <template #icon><t-icon name="close" /></template>
                驳回
              </t-button>
              <t-button
                size="small"
                variant="outline"
                :loading="acting.id === row.id && acting.kind === 'addsign'"
                :data-testid="`home-approval-addsign-${row.id}`"
                @click="openAddSign(row)"
              >
                <template #icon><t-icon name="user-add" /></template>
                加签
              </t-button>
            </div>
          </div>
        </li>
      </ul>

      <div v-else class="apv__empty" data-testid="home-approval-empty">
        <t-icon :name="denied ? 'lock-on' : 'inbox'" size="28px" />
        <span>{{ emptyText }}</span>
      </div>

      <!-- 底部统计：N = 后端 total（跨页真实总数），M = 今日已办（`since`=今天零点现算） -->
      <div v-if="!denied" class="apv__foot">
        <span class="apv__foot-text" data-testid="home-approval-stats">
          待办 <b>{{ total }}</b> 项
          <template v-if="doneToday !== null">
            <span class="apv__sep">·</span>
            今日已办 <b>{{ doneToday }}</b> 项
          </template>
        </span>
        <t-link theme="primary" hover="color" data-testid="home-approval-batch" @click="goFlowCenter">
          批量审批
          <t-icon name="chevron-right" />
        </t-link>
      </div>

      <div v-if="error" class="apv__error">
        <t-icon name="error-circle" />
        <span>{{ error }}</span>
        <t-link theme="primary" hover="color" @click="load()">重试</t-link>
      </div>
    </template>

    <!-- ① 同意：意见可空（后端也允许空意见） -->
    <t-dialog
      v-model:visible="approveVisible"
      header="同意审批"
      width="480px"
      :confirm-btn="{ content: '确认同意', theme: 'success', loading: submitting }"
      destroy-on-close
      :close-on-overlay-click="!submitting"
      data-testid="home-approval-approve-dialog"
      @confirm="submitApprove"
    >
      <p class="apv__dialog-target">{{ activeTarget }}</p>
      <t-form label-align="top">
        <t-form-item label="审批意见（可选）">
          <t-textarea
            v-model="approveComment"
            placeholder="可以补充一句说明（不填也能同意）"
            :autosize="{ minRows: 3, maxRows: 6 }"
            :maxlength="500"
            data-testid="home-approval-approve-comment"
          />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ② 驳回：意见必填（后端口径 ≥ 5 字，前端先拦一道） -->
    <t-dialog
      v-model:visible="rejectVisible"
      header="驳回审批"
      width="480px"
      :confirm-btn="{ content: '确认驳回', theme: 'danger', disabled: !rejectOk, loading: submitting }"
      destroy-on-close
      :close-on-overlay-click="!submitting"
      data-testid="home-approval-reject-dialog"
      @confirm="submitReject"
    >
      <p class="apv__dialog-target">{{ activeTarget }}</p>
      <t-form label-align="top">
        <t-form-item label="驳回意见（至少 5 个字，必填）" required>
          <t-textarea
            v-model="rejectComment"
            placeholder="说明驳回原因，便于发起人修改后重新提交"
            :autosize="{ minRows: 3, maxRows: 6 }"
            :maxlength="500"
            data-testid="home-approval-reject-comment"
          />
          <p class="apv__dialog-tip" :class="{ 'apv__dialog-tip--ok': rejectOk }">
            {{ rejectComment.trim().length }}/5 字
          </p>
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ③ 加签：复用任务中心的选人组件（一次一人，后端接口收 user_ids[]） -->
    <t-dialog
      v-model:visible="addSignVisible"
      header="加签"
      width="420px"
      :confirm-btn="{ content: '确认加签', theme: 'primary', disabled: !addSignUserId, loading: submitting }"
      destroy-on-close
      :close-on-overlay-click="!submitting"
      data-testid="home-approval-addsign-dialog"
      @confirm="submitAddSign"
    >
      <p class="apv__dialog-target">{{ activeTarget }}</p>
      <t-form label-align="top">
        <t-form-item label="加签人" required>
          <!-- `v-if`：UserSelect 首次打开会预取一页用户（`GET /system/user/page`）。
               不加这个判断时 t-dialog 即便隐藏也会挂载内容 → 没有 `system:user:list` 的账号
               一进首页就吃一个 403（实测非超管 console 报错）。只在真正打开加签框时才挂载。 -->
          <UserSelect v-if="addSignVisible" v-model="addSignUserId" placeholder="搜索用户名 / 姓名" />
        </t-form-item>
        <p class="apv__dialog-tip">加签后这张单仍需要你审批（多一个人会签）</p>
      </t-form>
    </t-dialog>
  </t-card>
</template>

<script setup lang="ts">
/**
 * 工作台「待我审批」卡（HOME-5）。
 *
 * # 数据源（全部是已有接口，不新增任何后端接口）
 *
 * | 用途 | 接口 | 取哪一段 |
 * |---|---|---|
 * | 待办列表（最多 10 条） | `GET /api/business/flow/task/todo?page=1&page_size=10` | `list` + `total` |
 * | 今日已办数 | `GET /api/business/flow/task/done?page=1&page_size=1&since=<今天零点 ISO>` | `total` |
 * | 同意 / 驳回 / 加签 | `POST /api/business/flow/task/{id}/{approve,reject,add-sign}` | — |
 *
 * 「今日已办」不能直接用 `done` 的 `total`（那是**全量历史**已办）：所以给 `done` 带上
 * `since`，由后端按 `completed_at >= since` 现算。本地零点是用户时区口径，
 * 用 `dayjs().startOf('day').toISOString()` 转成 UTC 发给后端。
 *
 * # 交互口径
 *
 * - **不做乐观更新**：三个动作都是等接口返回 0 才改 UI（成功后才刷新列表）；
 * - 同意 / 驳回成功后刷新整卡（该条从后端消失，`total` 与「今日已办」同步 +1）；
 * - 加签成功后刷新（该条保留），提示「已加签，仍需你审批」；
 * - 驳回意见 ≥ 5 字是**前端硬拦**（确认按钮禁用）+ 后端二次校验。
 *
 * # 跳转（点单据号）
 *
 * 走审批详情页同一份映射 `biz-link.ts`（`biz_type` → 业务详情页 + 深链键），
 * 未登记的类型 / 没有业务 id → 回落到审批单详情 `/business/flow/instance/{instance_id}`。
 * 不在本组件里另写一份「类型 → 路由」表。
 */
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import {
  addSignFlowTask,
  approveFlowTask,
  listDoneTasks,
  listTodoTasks,
  rejectFlowTask,
  type FlowTaskItem,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import { FLOW_BIZ_TYPE_LABELS, type FlowBizType } from '@/api/business/erp/doc-kinds';
import { bizLinkRouteOf, resolveBizLink } from '@/views/business/flow/instance/biz-link';
import UserSelect from '@/views/business/task/components/UserSelect.vue';
import { useUserStore } from '@/stores/user';

const router = useRouter();
const userStore = useUserStore();

/** 卡片最多展示几条（后端 total 另算，底部统计用真总数） */
const LIMIT = 10;
/** 审批中心路由（「全部」/「批量审批」都落这里，`?tab=todo` 直接停在待办 Tab） */
const FLOW_CENTER = '/flow/task';
/** 驳回意见最小字数（与后端 `normalize_reject_comment` 同口径） */
const REJECT_MIN_CHARS = 5;

/**
 * 审批类型 → 卡片上的「单据类型」短名。
 *
 * 设计稿样例是「采购订单 PO-…」而不是「采购订单审批 PO-…」，所以这里给的是**单据对象名**；
 * 表里没有的类型回落到 `FLOW_BIZ_TYPE_LABELS`（带「审批」二字）再回落到流程名。
 * 值是中文常量，不是配色，不涉及主题变量。
 */
const DOC_SHORT_LABELS: Readonly<Partial<Record<FlowBizType, string>>> = {
  PO_APPROVE: '采购订单',
  STOCK_IN_POST: '收货入库单',
  STOCK_IN_CANCEL: '收货作废',
  STOCK_TAKE_POST: '盘点单',
  STOCK_TRANSFER: '调拨单',
  PAYABLE_PAY: '付款单',
  PAYABLE_CANCEL: '应付作废',
  VOUCHER_REVERSE: '凭证红冲',
  CONTRACT_APPROVE: '合同',
  TASK_ACCEPT: '任务验收',
  FLOW_EDGE_CFG: '状态机配置',
  EXPENSE: '费用报销',
  BUDGET_APPROVE: '预算',
  ASSET_DISPOSAL: '资产处置',
  RECEIPT_REFUND: '收款退款',
};

/** 卡片一行（模板只取值，文案 / 金额 / 相对时间在 computed 里算好） */
interface ApprovalRow {
  /** 待办 id（三个动作都按它提交） */
  id: string;
  /** 实例 id（兜底跳转用） */
  instanceId: string;
  /** 单据类型中文名 */
  typeLabel: string;
  /** 展示用单号（业务单号优先，没有就用审批单号） */
  no: string;
  /** 提交人 */
  initiator: string;
  /** 相对时间文案 */
  relative: string;
  /** 金额文案（`null` = 该单据没有金额字段，不显示） */
  amount: string | null;
  /** hover 全文（单据标题 / 流程名 / 当前节点） */
  tooltip: string;
  /** 原始任务（点开单据详情要 biz_type / biz_id / biz_no） */
  task: FlowTaskItem;
}

/** 待办原始行 */
const tasks = ref<FlowTaskItem[]>([]);
/** 真实待办总数（跨页） */
const total = ref(0);
/** 今日已办数（`null` = 还没取到） */
const doneToday = ref<number | null>(null);
/** 加载中 */
const loading = ref(false);
/** 是否已成功拉过一次（决定首屏骨架） */
const loaded = ref(false);
/** 加载失败提示 */
const error = ref('');
/** 没有 `business:flow:instance:read`：卡片照常显示空态，但不发注定 403 的请求 */
const denied = ref(false);

/** 同意对话框 */
const approveVisible = ref(false);
/** 同意意见（可空） */
const approveComment = ref('');
/** 驳回对话框 */
const rejectVisible = ref(false);
/** 驳回意见（≥ 5 字） */
const rejectComment = ref('');
/** 加签对话框 */
const addSignVisible = ref(false);
/** 加签人 id */
const addSignUserId = ref<string | null>(null);
/** 正在提交（对话框确认按钮 loading） */
const submitting = ref(false);
/** 当前操作的待办（三个对话框共用） */
const activeTask = ref<FlowTaskItem | null>(null);
/** 行内按钮 loading（按 id + 动作区分，只转被点的那一个） */
const acting = ref<{ id: string; kind: 'approve' | 'reject' | 'addsign' | '' }>({ id: '', kind: '' });

/** 当前操作目标的展示文案（对话框里回显，避免误批） */
const activeTarget = computed(() => {
  const task = activeTask.value;
  if (!task) return '';
  return `${docLabel(task)} ${docNo(task)} · 提交人 ${task.initiator_name || '—'}`;
});

/** 驳回意见是否达标（按钮禁用 + 提示色） */
const rejectOk = computed(() => rejectComment.value.trim().length >= REJECT_MIN_CHARS);

/** 空态文案：没权限 ≠ 没有待办，两句话不能混 */
const emptyText = computed(() => (denied.value ? '当前账号没有审批查看权限' : '没有待我审批的单据'));

/** 卡片行（`FlowTaskItem` → 展示行） */
const rows = computed<ApprovalRow[]>(() =>
  tasks.value.map((task) => ({
    id: task.id,
    instanceId: task.instance_id,
    typeLabel: docLabel(task),
    no: docNo(task),
    initiator: task.initiator_name || '—',
    relative: relativeTime(task.arrived_at),
    amount: amountText(task),
    tooltip: [task.title, task.flow_name, task.current_node_name].filter(Boolean).join(' · ') || docNo(task),
    task,
  })),
);

/** 单据类型中文名（`biz_type` → 短名 → 审批类型全名 → 流程名） */
function docLabel(task: FlowTaskItem): string {
  const bizType = (task.biz_type || '').toUpperCase();
  if (!bizType) return task.flow_name || '审批单';
  const short = DOC_SHORT_LABELS[bizType as FlowBizType];
  if (short) return short;
  const full = FLOW_BIZ_TYPE_LABELS[bizType as FlowBizType];
  return full || task.flow_name || '审批单';
}

/** 展示单号：业务单号（`PO-…`）优先；没有业务单号时退回审批单号（`FLW-…`） */
function docNo(task: FlowTaskItem): string {
  return task.biz_no || task.instance_no || '—';
}

/**
 * 金额：取 `form_data.amount`（采购 / 报销 / 合同的表单快照里都有这一项，是 JSON 数字或数字字符串）。
 *
 * 没有这个键就**不显示**金额（通用流程 / 任务验收没有金额，编一个「¥0」是假信息）。
 */
function amountText(task: FlowTaskItem): string | null {
  const form = task.form_data || {};
  const raw = form['amount'];
  if (raw === null || raw === undefined || raw === '') return null;
  const value = typeof raw === 'number' ? raw : Number(String(raw));
  if (!Number.isFinite(value)) return null;
  return `¥${value.toLocaleString('zh-CN', { minimumFractionDigits: 0, maximumFractionDigits: 2 })}`;
}

/**
 * 相对时间：刚刚 / N 分钟前 / N 小时前 / N 天前；超过 7 天显示日期。
 *
 * 未来时间（时钟偏差）按「刚刚」处理，不显示「-3 分钟前」这种怪东西。
 */
function relativeTime(value: string): string {
  if (!value) return '—';
  const parsed = dayjs(value);
  if (!parsed.isValid()) return value;
  const minutes = dayjs().diff(parsed, 'minute');
  if (minutes < 1) return '刚刚';
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days} 天前`;
  return parsed.format('YYYY-MM-DD');
}

/** 拉数据：待办 10 条 + 今日已办数（两路并发，任一路失败不影响另一路） */
async function load(options: { silent?: boolean } = {}): Promise<void> {
  if (!userStore.hasPermission('business:flow:instance:read')) {
    denied.value = true;
    loaded.value = true;
    tasks.value = [];
    total.value = 0;
    return;
  }
  if (!options.silent) loading.value = true;
  error.value = '';
  // 今日零点（本地时区）→ UTC ISO；后端按 `completed_at >= since` 现算
  const since = dayjs().startOf('day').toISOString();
  const [todoRes, doneRes] = await Promise.allSettled([
    listTodoTasks({ page: 1, page_size: LIMIT }),
    listDoneTasks({ page: 1, page_size: 1, since }),
  ]);

  if (todoRes.status === 'fulfilled') {
    tasks.value = (todoRes.value.list ?? []).slice(0, LIMIT);
    total.value = todoRes.value.total ?? tasks.value.length;
    denied.value = false;
  } else {
    const reason = todoRes.reason;
    tasks.value = [];
    total.value = 0;
    if (reason instanceof ApiError && reason.status === 403) {
      denied.value = true;
    } else {
      error.value = reason instanceof ApiError ? reason.message : '待办审批加载失败';
    }
  }

  doneToday.value = doneRes.status === 'fulfilled' ? (doneRes.value.total ?? 0) : null;

  loading.value = false;
  loaded.value = true;
}

/** 打开业务详情：映射表来自 `biz-link.ts`，未登记类型回落审批单详情 */
function openDoc(row: ApprovalRow): void {
  const task = row.task;
  const target = resolveBizLink(task.biz_type || '', {
    id: task.biz_id || '',
    no: task.biz_no || '',
  });
  const route = target ? bizLinkRouteOf(target, { id: task.biz_id || '', no: task.biz_no || '' }) : null;
  if (route) {
    void router.push(route);
    return;
  }
  void router.push(`/business/flow/instance/${task.instance_id}`);
}

/** 打开同意对话框 */
function openApprove(row: ApprovalRow): void {
  activeTask.value = row.task;
  approveComment.value = '';
  approveVisible.value = true;
}

/** 打开驳回对话框 */
function openReject(row: ApprovalRow): void {
  activeTask.value = row.task;
  rejectComment.value = '';
  rejectVisible.value = true;
}

/** 打开加签对话框 */
function openAddSign(row: ApprovalRow): void {
  activeTask.value = row.task;
  addSignUserId.value = null;
  addSignVisible.value = true;
}

/** 409 = 已被人处理 / 已结束：不是「操作失败」，刷新让用户看到最新列表 */
function handleActionError(taskId: string, err: unknown, fallback: string): void {
  if (err instanceof ApiError && err.status === 409) {
    void MessagePlugin.warning('这条审批已被处理或已结束，列表已刷新');
    void load({ silent: true });
    return;
  }
  void MessagePlugin.error(err instanceof ApiError ? err.message : fallback);
  acting.value = { id: taskId, kind: '' };
}

/** 提交同意（等接口返回才关框 + 刷新，不做乐观更新） */
async function submitApprove(): Promise<void> {
  const task = activeTask.value;
  if (!task || submitting.value) return;
  submitting.value = true;
  acting.value = { id: task.id, kind: 'approve' };
  try {
    await approveFlowTask(task.id, { comment: approveComment.value.trim() || undefined });
    approveVisible.value = false;
    void MessagePlugin.success('已同意');
    await load({ silent: true });
  } catch (err) {
    handleActionError(task.id, err, '同意失败');
  } finally {
    submitting.value = false;
    acting.value = { id: '', kind: '' };
  }
}

/** 提交驳回（意见必填 ≥ 5 字，前端已禁用按钮，这里再兜一道） */
async function submitReject(): Promise<void> {
  const task = activeTask.value;
  if (!task || submitting.value) return;
  const comment = rejectComment.value.trim();
  if (comment.length < REJECT_MIN_CHARS) {
    void MessagePlugin.warning(`驳回意见至少 ${REJECT_MIN_CHARS} 个字`);
    return;
  }
  submitting.value = true;
  acting.value = { id: task.id, kind: 'reject' };
  try {
    await rejectFlowTask(task.id, { comment });
    rejectVisible.value = false;
    void MessagePlugin.success('已驳回');
    await load({ silent: true });
  } catch (err) {
    handleActionError(task.id, err, '驳回失败');
  } finally {
    submitting.value = false;
    acting.value = { id: '', kind: '' };
  }
}

/** 提交加签（成功后该条保留，加签完还要自己批） */
async function submitAddSign(): Promise<void> {
  const task = activeTask.value;
  if (!task || submitting.value) return;
  if (!addSignUserId.value) {
    void MessagePlugin.warning('请先选择加签人');
    return;
  }
  submitting.value = true;
  acting.value = { id: task.id, kind: 'addsign' };
  try {
    await addSignFlowTask(task.id, { user_ids: [addSignUserId.value] });
    addSignVisible.value = false;
    void MessagePlugin.success('已加签，仍需你继续审批');
    await load({ silent: true });
  } catch (err) {
    handleActionError(task.id, err, '加签失败');
  } finally {
    submitting.value = false;
    acting.value = { id: '', kind: '' };
  }
}

/** 跳审批中心（待办 Tab） */
function goFlowCenter(): void {
  void router.push({ path: FLOW_CENTER, query: { tab: 'todo' } });
}

onMounted(() => {
  void load();
});
</script>

<style scoped lang="less">
// 骨架屏流光（@keyframes 必须顶层，嵌在选择器里 LESS 不会提升）
@keyframes apv-shimmer {
  from {
    background-position: 100% 0;
  }

  to {
    background-position: 0 0;
  }
}

.apv {
  // t-card 默认 24px 内边距；条目行自带留白，收紧两侧
  :deep(.t-card__body) {
    padding: 6px 12px 8px;
  }

  &__skeleton {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 8px 4px;
  }

  &__skeleton-row {
    height: 38px;
    background: linear-gradient(
      90deg,
      var(--td-bg-color-container-hover) 25%,
      var(--td-bg-color-secondarycontainer) 37%,
      var(--td-bg-color-container-hover) 63%
    );
    background-size: 400% 100%;
    border-radius: var(--td-radius-default);
    animation: apv-shimmer 1.4s ease infinite;
  }

  &__list {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  // 一行 = 左侧状态点 + 右主体（标题 / 元信息 / 按钮三行）
  &__item {
    display: flex;
    gap: 10px;
    padding: 10px 6px 12px;
    border-radius: var(--td-radius-default);
    transition: background-color 0.15s ease;

    & + & {
      border-top: 1px solid var(--td-component-stroke);
    }

    &:hover {
      background-color: var(--td-bg-color-container-hover);
    }
  }

  // 待审状态点：主题色实心圆（语义固定，跟配色方案走）
  &__dot {
    flex: none;
    width: 7px;
    height: 7px;
    margin-top: 7px;
    background: var(--td-brand-color);
    border-radius: 50%;
    box-shadow: 0 0 0 3px var(--td-brand-color-light);
  }

  &__main {
    display: flex;
    flex: 1 1 auto;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  // 标题区：button 重置成文本行（键盘可达，focus-visible 保留焦点环）
  &__title {
    display: flex;
    gap: 6px;
    align-items: baseline;
    width: 100%;
    padding: 0;
    font: inherit;
    text-align: left;
    cursor: pointer;
    background: none;
    border: 0;
    border-radius: var(--td-radius-small);

    &:hover .apv__no {
      color: var(--td-brand-color);
      text-decoration: underline;
      text-underline-offset: 2px;
    }

    &:focus-visible {
      outline: 2px solid var(--td-brand-color);
      outline-offset: 2px;
    }
  }

  &__type {
    flex: none;
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  // 单号等宽：对照 ERP 单号时数字不会「跳」。
  // TDesign 没有等宽字体变量，用系统等宽栈（不引外部字体）。
  &__no {
    overflow: hidden;
    font-family: var(--td-font-family-mono, 'SFMono-Regular', 'Menlo', 'Consolas', monospace);
    font-size: 13px;
    color: var(--td-text-color-secondary);
    text-overflow: ellipsis;
    white-space: nowrap;
    transition: color 0.15s ease;
  }

  &__meta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: baseline;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__who {
    color: var(--td-text-color-secondary);
  }

  &__sep {
    color: var(--td-text-color-placeholder);
  }

  &__time {
    font-variant-numeric: tabular-nums;
  }

  &__amount {
    font-weight: 600;
    color: var(--td-text-color-primary);
    font-variant-numeric: tabular-nums;
  }

  &__actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;

    // 三个小按钮按内容宽度，不撑满整行
    :deep(.t-button) {
      flex: none;
    }
  }

  &__empty {
    display: flex;
    flex-direction: column;
    gap: 10px;
    align-items: center;
    justify-content: center;
    padding: 28px 0 24px;
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }

  &__foot {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
    padding: 10px 6px 2px;
    margin-top: 4px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    border-top: 1px solid var(--td-component-stroke);

    b {
      font-weight: 600;
      color: var(--td-text-color-primary);
      font-variant-numeric: tabular-nums;
    }
  }

  &__error {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 8px 6px 2px;
    font-size: 12px;
    color: var(--td-error-color);
  }

  &__dialog-target {
    padding: 8px 10px;
    margin: 0 0 12px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
    background: var(--td-bg-color-container-hover);
    border-radius: var(--td-radius-default);
  }

  &__dialog-tip {
    margin: 4px 0 0;
    font-size: 12px;
    color: var(--td-text-color-placeholder);

    &--ok {
      color: var(--td-success-color);
    }
  }
}
</style>
