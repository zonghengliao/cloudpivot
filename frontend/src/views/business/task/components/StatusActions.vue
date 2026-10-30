<template>
  <!--
    状态流转按钮组（E5）。

    - **没有可做的流转就整块不渲染**（不是画一个灰按钮）：白名单 10 条 + 角色位
      掩码 + `need_review` 三重过滤后为空 → 连这一层 `<div>` 都不存在。
    - 打回（REVIEW → DOING）先弹窗填原因（≥5 字），不填不发请求。
  -->
  <div v-if="actions.length > 0" class="status-actions" data-testid="task-status-actions">
    <t-button
      v-for="action in actions"
      :key="action.key"
      size="small"
      variant="outline"
      :theme="action.theme"
      :loading="pending.key === action.key"
      :disabled="pending.key !== '' && pending.key !== action.key"
      :data-action="action.key"
      @click="trigger(action)"
    >
      {{ action.label }}
    </t-button>
  </div>

  <!-- 打回原因弹窗：空或 <5 字 → 确定按钮 disabled（红线） -->
  <t-dialog
    v-model:visible="reject.visible"
    header="打回原因"
    width="480px"
    :footer="false"
    :close-on-overlay-click="false"
    @close="closeReject"
  >
    <p class="reject__tip">
      任务「{{ task.title }}」从<b>待验收</b>打回<b>进行中</b>，必须填写原因（至少 5 个字），原因会记入任务时间线。
    </p>
    <t-textarea
      v-model="reject.text"
      :maxlength="500"
      show-limit
      :autosize="{ minRows: 3, maxRows: 6 }"
      placeholder="例如：验收标准未对齐，单测未覆盖"
      data-testid="reject-reason-input"
    />
    <p class="reject__hint">当前 {{ rejectText.length }} / 5 字</p>
    <p v-if="reject.error" class="reject__error">{{ reject.error }}</p>
    <div class="reject__actions">
      <t-button variant="outline" @click="closeReject">取消</t-button>
      <t-button
        theme="primary"
        :loading="reject.submitting"
        :disabled="!rejectCanSubmit"
        data-testid="reject-confirm"
        @click="submitReject"
      >
        确定
      </t-button>
    </div>
  </t-dialog>
</template>

<script setup lang="ts">
/**
 * 任务状态流转按钮（批次 C+ · E5）。
 *
 * # 为什么单独一个组件
 *
 * `detail.vue` 已经 1400+ 行（信息区 / 协作区 / 工时），再把「白名单表 + 角色位 +
 * 打回弹窗 + 错误码文案」塞进去会让那个文件彻底不可读。这里只做**一件事**：
 * 给定任务与当前用户，算出「现在能点哪几个按钮」，点完把回执抛给父组件刷新。
 *
 * # 判据全部来自后端，前端只是提前拦
 *
 * 与 `backend/src/business/task/state.rs` 的 `TRANSITIONS` 逐条对齐（10 条）：
 *
 * | # | from → to | 角色位 | 附加条件 |
 * |---|---|---|---|
 * | 1 | TODO → DOING | 负责人 / 分派人 / 创建人 / 管理员 | 前置依赖由后端校验 |
 * | 2 | DOING → REVIEW | 负责人 | — |
 * | 3 | DOING → DONE | 负责人 / 创建人 | 仅 `need_review=false` |
 * | 4 | REVIEW → DONE | 创建人 / 分派人 / 管理员（验收人） | — |
 * | 5 | REVIEW → DOING | 创建人 / 分派人 / 管理员 | 必填原因 ≥5 字 |
 * | 6 | DONE → DOING | 创建人 / 管理员（重开） | — |
 * | 7–9 | TODO / DOING / REVIEW → CANCELLED | 负责人 / 创建人 / 分派人 / 管理员 | — |
 * | 10 | CANCELLED → TODO | 创建人 / 管理员（恢复） | — |
 *
 * **`DONE` 没有取消出边**（必须先重开），前端也不画那个按钮 —— 少一个「点了必 409」的按钮。
 *
 * # 显隐 = 权限码 **且** 角色位（红线：不给 disabled，直接不显示）
 *
 * 接口层是两道门：`#[nik::permission("business:task:status")]`（功能级）+
 * `TaskPeople::role_mask`（行级）。所以：
 *
 * - 没有 `business:task:status` → **一个按钮都不显示**（点必 403）；
 * - 有码但角色位不命中（典型：协作人 / 无关同事）→ 同样不显示。
 *
 * 管理员位（8）按 `SUPER_ADMIN` 角色叠加 —— 与后端 `ScopedPrincipal::is_super`
 * （`role_code == "SUPER_ADMIN"`）同口径，**不是**按角色名猜权限。
 *
 * # 65030：审批门闸要给出路
 *
 * 任务挂了审批单且实例 `PENDING` 时，`REVIEW → DONE / DOING` 会被后端拦成
 * `409 + 65030`。这时光提示「失败」没有意义 —— 结论只能在审批中心下，
 * 所以文案里带一个**去审批中心**的链接（有实例侧权限码才给链接，没有就只给文案，
 * 避免把人送到 403 页）。
 */
import { computed, h, reactive } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  changeTaskStatus,
  TASK_ERROR_CODE,
  type TaskDetail,
  type TaskStatus,
  type TaskStatusChangeResult,
} from '@/api/business/task-api';
import { PERM } from '@/constants/permissions';
import { statusLabel } from '@/constants/status-map';
import { useUserStore } from '@/stores/user';
import { useBadgeStore } from '@/stores/badge';

const props = defineProps<{
  /** 当前任务详情（只读；流转后由父组件重新拉） */
  task: TaskDetail;
}>();

const emit = defineEmits<{
  /** 流转成功（父组件据此刷新详情 + 列表） */
  (event: 'changed', result: TaskStatusChangeResult): void;
}>();

const userStore = useUserStore();
const router = useRouter();

/** 角色位（与后端 `state.rs` 的 `ROLE_*` 常量逐位一致） */
const ROLE_OWNER = 1;
const ROLE_ASSIGNER = 2;
const ROLE_CREATOR = 4;
const ROLE_ADMIN = 8;

/** 打回原因最少字数（与后端 `MIN_REJECT_REASON_CHARS` 同口径） */
const MIN_REJECT_REASON = 5;

/** 一条可点的流转 */
interface StatusAction {
  /** 稳定标识（按钮 `data-action` + loading 定位） */
  key: string;
  /** 按钮文案 */
  label: string;
  /** 目标状态 */
  to: TaskStatus;
  /** 按钮主题（前进动作用 primary，回退 / 终止用 warning / danger） */
  theme: 'default' | 'primary' | 'success' | 'warning' | 'danger';
}

/** 白名单（顺序 = 按钮从左到右的展示顺序） */
interface TransitionRule {
  from: TaskStatus;
  to: TaskStatus;
  key: string;
  label: string;
  theme: StatusAction['theme'];
  /** 允许的角色位 */
  actors: number;
  /** 仅 `need_review = false` 时可用 */
  requiresNoReview?: boolean;
  /** 必填原因（弹窗） */
  requiresReason?: boolean;
  /** 点之前二次确认的文案（不可回的流转才要） */
  confirm?: string;
}

const RULES: TransitionRule[] = [
  {
    from: 'TODO',
    to: 'DOING',
    key: 'start',
    label: '开始处理',
    theme: 'primary',
    actors: ROLE_OWNER | ROLE_ASSIGNER | ROLE_CREATOR | ROLE_ADMIN,
  },
  {
    from: 'DOING',
    to: 'REVIEW',
    key: 'submit-review',
    label: '提交验收',
    theme: 'primary',
    actors: ROLE_OWNER,
  },
  {
    from: 'DOING',
    to: 'DONE',
    key: 'direct-done',
    label: '直接完成',
    theme: 'success',
    actors: ROLE_OWNER | ROLE_CREATOR,
    requiresNoReview: true,
    confirm: '该任务免验收，确认直接标记完成？',
  },
  {
    from: 'REVIEW',
    to: 'DONE',
    key: 'accept',
    label: '验收通过',
    theme: 'success',
    actors: ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
    confirm: '确认验收通过？通过后任务进入已完成。',
  },
  {
    from: 'REVIEW',
    to: 'DOING',
    key: 'reject',
    label: '打回',
    theme: 'warning',
    actors: ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
    requiresReason: true,
  },
  {
    from: 'DONE',
    to: 'DOING',
    key: 'reopen',
    label: '重开',
    theme: 'default',
    actors: ROLE_CREATOR | ROLE_ADMIN,
    confirm: '重开会把任务退回进行中、进度回退并清空完成时间，确认？',
  },
  {
    from: 'TODO',
    to: 'CANCELLED',
    key: 'cancel',
    label: '取消',
    theme: 'danger',
    actors: ROLE_OWNER | ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
    confirm: '确认取消该任务？取消后仍可恢复为待开始。',
  },
  {
    from: 'DOING',
    to: 'CANCELLED',
    key: 'cancel',
    label: '取消',
    theme: 'danger',
    actors: ROLE_OWNER | ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
    confirm: '确认取消该任务？取消后仍可恢复为待开始。',
  },
  {
    from: 'REVIEW',
    to: 'CANCELLED',
    key: 'cancel',
    label: '取消',
    theme: 'danger',
    actors: ROLE_OWNER | ROLE_CREATOR | ROLE_ASSIGNER | ROLE_ADMIN,
    confirm: '确认取消该任务？取消后仍可恢复为待开始。',
  },
  {
    from: 'CANCELLED',
    to: 'TODO',
    key: 'restore',
    label: '恢复',
    theme: 'default',
    actors: ROLE_CREATOR | ROLE_ADMIN,
  },
];

/** 是否管理员（超管）—— 与后端 `role_code == "SUPER_ADMIN"` 同口径 */
const isAdmin = computed(
  () => userStore.roles.includes('SUPER_ADMIN') || userStore.permissions.includes('*:*:*'),
);

/** 我在这条任务上的角色位（一人多角色取并集，管理员位独立叠加） */
const roleMask = computed(() => {
  const myId = userStore.userInfo?.id ?? '';
  const task = props.task;
  let mask = 0;
  if (myId && task.owner_id === myId) {
    mask |= ROLE_OWNER;
  }
  if (myId && task.creator_id === myId) {
    mask |= ROLE_CREATOR;
  }
  if (myId && task.assigner_id && task.assigner_id === myId) {
    mask |= ROLE_ASSIGNER;
  }
  if (isAdmin.value) {
    mask |= ROLE_ADMIN;
  }
  return mask;
});

/** 是否有改状态的功能级权限（没这个码 → 后端必 403，前端一个按钮都不画） */
const canChangeStatus = computed(() => userStore.hasPermission(PERM.taskStatus));

/** 当前状态（后端下发字符串，统一大写后比对） */
const currentStatus = computed<TaskStatus>(
  () => (props.task.status || 'TODO').toUpperCase() as TaskStatus,
);

/** 现在能点的按钮（三重过滤：白名单 → 角色位 → need_review） */
const actions = computed<StatusAction[]>(() => {
  if (!canChangeStatus.value || roleMask.value === 0) {
    return [];
  }
  return RULES.filter((rule) => {
    if (rule.from !== currentStatus.value) {
      return false;
    }
    if (rule.requiresNoReview && props.task.need_review) {
      return false;
    }
    return (roleMask.value & rule.actors) !== 0;
  }).map((rule) => ({ key: rule.key, label: rule.label, to: rule.to, theme: rule.theme }));
});

/** 正在提交的按钮 key（为空 = 空闲） */
const pending = reactive({ key: '' });
const badgeStore = useBadgeStore();

/** 打回弹窗状态 */
const reject = reactive({
  visible: false,
  text: '',
  error: '',
  submitting: false,
});

/** 打回原因（去掉首尾空白后的字数判据，与提交时一致） */
const rejectText = computed(() => reject.text.trim());

/** 确定按钮是否可点（<5 字一律 disabled） */
const rejectCanSubmit = computed(() => rejectText.value.length >= MIN_REJECT_REASON);

/** 点按钮：必填原因的走弹窗，需要二次确认的先确认，其余直接发 */
function trigger(action: StatusAction): void {
  if (pending.key !== '') {
    return;
  }
  const rule = RULES.find((item) => item.from === currentStatus.value && item.key === action.key);
  if (!rule) {
    return;
  }
  if (rule.requiresReason) {
    reject.text = '';
    reject.error = '';
    reject.submitting = false;
    reject.visible = true;
    return;
  }
  if (rule.confirm) {
    const dialog = DialogPlugin.confirm({
      header: action.label,
      body: rule.confirm,
      theme: action.theme === 'danger' ? 'danger' : 'warning',
      confirmBtn: { content: action.label, theme: action.theme === 'danger' ? 'danger' : 'primary' },
      onConfirm: () => {
        dialog.destroy();
        void changeStatus(action.to, '', action.key);
      },
    });
    return;
  }
  void changeStatus(action.to, '', action.key);
}

/** 关闭打回弹窗（不发请求，状态与按钮保持原样） */
function closeReject(): void {
  reject.visible = false;
  reject.error = '';
  reject.submitting = false;
}

/** 提交打回：本地先拦 <5 字（按钮 disabled 是第二道保险） */
async function submitReject(): Promise<void> {
  if (!rejectCanSubmit.value || reject.submitting) {
    return;
  }
  const text = rejectText.value;
  reject.submitting = true;
  const ok = await changeStatus('DOING', text, 'reject');
  reject.submitting = false;
  if (ok) {
    reject.visible = false;
  }
}

/**
 * 真正发请求 + 按错误码说话。
 *
 * 判据只能是**业务码**（`api/request.ts` 已把 HTTP 层规整成 `ApiError`）：
 * `63010` 非法流转 / `63011` 无权 / `63013` 缺原因 / `63006` 有未完成子任务 /
 * `65030` 审批中（这条要走「给出路」的分支）。
 *
 * # 返回值
 * 成功 `true`；失败 `false`（调用方据此决定是否关弹窗 —— 失败时保留用户输入）。
 */
async function changeStatus(to: TaskStatus, reason: string, key: string): Promise<boolean> {
  pending.key = key;
  try {
    const result = await changeTaskStatus(props.task.id, to, reason || undefined);
    MessagePlugin.success(`已流转到「${statusLabel('task', result.status)}」`);
    emit('changed', result);
    void badgeStore.refresh();
    return true;
  } catch (error) {
    if (error instanceof ApiError && error.code === TASK_ERROR_CODE.APPROVAL_PENDING) {
      notifyApprovalPending();
      return false;
    }
    if (error instanceof ApiError && error.code === TASK_ERROR_CODE.REJECT_REASON_REQUIRED) {
      reject.error = '打回必须填写原因（至少 5 个字）';
      if (!reject.visible) {
        reject.text = '';
        reject.visible = true;
      }
      return false;
    }
    MessagePlugin.error(transitionErrorText(error));
    return false;
  } finally {
    pending.key = '';
  }
}

/**
 * `65030` 审批门闸：提示 + 去审批中心的链接。
 *
 * 链接用 `t-message` 的 vnode 插槽渲染（`content` 传函数），**不用** `window.open`：
 * 应用是 SPA，审批中心需要走 vue-router 才能保住登录态与面包屑。
 * 没有 `business:flow:instance:read` 时只给文案 —— 送去 `/flow/task` 只会撞 403。
 */
function notifyApprovalPending(): void {
  const canOpenCenter = userStore.hasPermission(PERM.flowInstanceRead);
  MessagePlugin.warning({
    duration: 8000,
    content: () =>
      h('span', { 'data-testid': 'approval-pending-tip' }, [
        '该任务正在走审批，请到「审批中心 → 待我审批」处理',
        canOpenCenter
          ? h(
              'a',
              {
                'data-testid': 'approval-pending-link',
                style: 'margin-left: 8px; color: var(--td-brand-color); cursor: pointer; text-decoration: underline;',
                onClick: () => {
                  void router.push({ path: '/flow/task', query: { tab: 'todo' } });
                },
              },
              '去审批中心',
            )
          : null,
      ]),
  });
}

/** 错误码 → 具体文案（红线：不许只说「操作失败」） */
function transitionErrorText(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case TASK_ERROR_CODE.TRANSITION_INVALID:
        return '该状态流转不被允许（页面状态可能已过期，请刷新后重试）';
      case TASK_ERROR_CODE.STATUS_FORBIDDEN:
        return '你没有权限执行该流转（待验收需验收人操作）';
      case TASK_ERROR_CODE.REJECT_REASON_REQUIRED:
        return '打回必须填写原因（至少 5 个字）';
      case TASK_ERROR_CODE.HAS_OPEN_CHILDREN:
        return '该任务还有未完成的子任务，不能标记完成';
      case TASK_ERROR_CODE.NOT_FOUND:
        return '任务不存在或你已无权查看，请刷新列表';
      default:
        return error.message || '状态流转失败，请稍后重试';
    }
  }
  return '状态流转失败，请检查网络后重试';
}
</script>

<style scoped lang="less">
.status-actions {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.reject {
  &__tip {
    margin: 0 0 10px;
    font-size: 13px;
    line-height: 1.7;
    color: var(--td-text-color-secondary);

    b {
      color: var(--td-text-color-primary);
    }
  }

  &__hint {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__error {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--td-error-color);
  }

  &__actions {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
}
</style>
