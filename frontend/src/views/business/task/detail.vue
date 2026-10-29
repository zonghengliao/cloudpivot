<template>
  <t-drawer
    v-model:visible="innerVisible"
    :header="drawerHeader"
    size="680px"
    :footer="false"
    class="task-detail"
    @closed="handleClosed"
  >
    <t-loading :loading="loading" size="small">
      <!-- 错误态：不白屏，给重试 -->
      <t-alert v-if="errorMessage" theme="error" :message="errorMessage" class="task-detail__alert">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadDetail">重试</t-button>
        </template>
      </t-alert>

      <template v-else-if="detail">
        <!-- 头部：编号 + 标题 + 徽标 + 操作 -->
        <div class="head">
          <div class="head__title-row">
            <span class="head__no">{{ detail.task_no }}</span>
            <span class="head__title">{{ detail.title }}</span>
          </div>
          <div class="head__meta">
            <StatusBadge domain="task" :status="detail.status" />
            <PriorityTag :priority="detail.priority" />
            <t-tag v-if="detail.need_review" size="small" variant="outline" theme="default">需验收</t-tag>
            <t-tag v-if="isOverdue" size="small" variant="light" theme="danger">已逾期</t-tag>
            <t-tag v-if="detail.source !== 'SELF'" size="small" variant="outline" theme="default">
              {{ sourceLabel(detail.source) }}
            </t-tag>
          </div>
          <div class="head__actions">
            <!-- 状态流转按钮：T2（状态机）未就绪，先画出来置灰，接口见 api/business/task-api.ts 末尾 TODO -->
            <t-tooltip content="状态流转功能即将上线" placement="top">
              <span class="head__disabled-group">
                <t-button size="small" variant="outline" disabled>开始处理</t-button>
                <t-button size="small" variant="outline" disabled>提交验收</t-button>
                <t-button size="small" variant="outline" disabled>完成</t-button>
                <t-button size="small" variant="outline" disabled>取消</t-button>
              </span>
            </t-tooltip>
            <t-space size="small">
              <t-button
                v-privilege="[PERM.taskUpdate]"
                size="small"
                theme="primary"
                variant="outline"
                @click="emit('edit', detail.id)"
              >
                编辑
              </t-button>
              <t-button v-privilege="[PERM.taskDelete]" size="small" theme="danger" variant="outline" @click="confirmRemove">
                删除
              </t-button>
            </t-space>
          </div>
        </div>

        <!-- 进度 -->
        <div class="block">
          <div class="block__title">进度</div>
          <div class="progress-row">
            <t-progress :percentage="detail.progress" :label="`${detail.progress}%`" theme="line" />
            <span class="progress-row__hours">
              预估 {{ formatHours(detail.estimate_hours) }}<template v-if="detail.actual_hours !== null">
                · 实际 {{ formatHours(detail.actual_hours) }}</template
              >
            </span>
          </div>
        </div>

        <!-- 信息区 -->
        <div class="block">
          <div class="block__title">基本信息</div>
          <dl class="info">
            <div class="info__item">
              <dt>负责人</dt>
              <dd>{{ detail.owner_name || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>创建人</dt>
              <dd>{{ detail.creator_name || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>分派人</dt>
              <dd>{{ detail.assigner_name || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>部门</dt>
              <dd>{{ detail.dept_name || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>计划开始</dt>
              <dd>{{ detail.start_date || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>截止日期</dt>
              <dd :class="{ 'info__value--overdue': isOverdue }">{{ detail.due_date || '无期限' }}</dd>
            </div>
            <div class="info__item">
              <dt>创建时间</dt>
              <dd>{{ formatDateTime(detail.created_at) }}</dd>
            </div>
            <div class="info__item">
              <dt>最近更新</dt>
              <dd>{{ formatDateTime(detail.updated_at) }}</dd>
            </div>
            <div v-if="detail.done_at" class="info__item">
              <dt>完成时间</dt>
              <dd>{{ formatDateTime(detail.done_at) }}</dd>
            </div>
            <div v-if="detail.parent_task_no" class="info__item">
              <dt>父任务</dt>
              <dd>{{ detail.parent_task_no }}</dd>
            </div>
          </dl>
          <t-alert v-if="detail.cancel_reason" theme="warning" :message="`取消原因：${detail.cancel_reason}`" />
          <t-alert v-if="detail.reject_reason" theme="error" :message="`打回原因：${detail.reject_reason}`" />
        </div>

        <!-- 描述 -->
        <div class="block">
          <div class="block__title">描述</div>
          <pre v-if="detail.description" class="desc">{{ detail.description }}</pre>
          <p v-else class="placeholder">暂无描述</p>
        </div>

        <!-- 子任务（T1 已有 children 字段；为空时整块隐藏） -->
        <div v-if="detail.children.length > 0" class="block">
          <div class="block__title">
            子任务
            <span class="block__count">{{ detail.children.length }}</span>
          </div>
          <div class="subtask">
            <div v-for="child in detail.children" :key="child.id" class="subtask__item">
              <StatusBadge domain="task" :status="child.status" :show-dot="false" />
              <span class="subtask__title">{{ child.title }}</span>
              <span class="subtask__meta">{{ child.owner_name }}</span>
              <span class="subtask__meta">{{ child.progress }}%</span>
            </div>
          </div>
        </div>

        <!-- 协作人（T1 详情已下发 members；为空时隐藏） -->
        <div v-if="detail.members.length > 0" class="block">
          <div class="block__title">
            协作人
            <span class="block__count">{{ detail.members.length }}</span>
          </div>
          <t-space size="small" break-line>
            <t-tag v-for="member in detail.members" :key="member.user_id" size="small" variant="outline">
              {{ member.user_name }}
            </t-tag>
          </t-space>
        </div>

        <!-- 附件（T1 只下发绑定关系，无下载地址；为空时隐藏） -->
        <div v-if="detail.attachments.length > 0" class="block">
          <div class="block__title">
            附件
            <span class="block__count">{{ detail.attachments.length }}</span>
          </div>
          <div v-for="file in detail.attachments" :key="file.id" class="attachment">
            <t-icon name="file" />
            <span class="attachment__name">{{ file.file_name }}</span>
            <span class="attachment__size">{{ formatFileSize(file.file_size) }}</span>
          </div>
        </div>

        <!--
          评论 / 附件上传 / 依赖 / 工时记录：T1 没有对应接口（评论属 T3），本期整块不渲染。
          TODO(批次C+·T3)：`GET /business/task/{id}/comments` 就绪后在此加时间线。
        -->
      </template>
    </t-loading>
  </t-drawer>
</template>

<script setup lang="ts">
/**
 * 任务详情（抽屉形态：列表页点标题 / 「查看」进来）。
 *
 * # 为什么是抽屉而不是独立页
 *
 * 任务详情的信息量不大（<1 屏），抽屉能保留列表的筛选与滚动位置，
 * 关掉抽屉不用重新查询 —— 运营场景里「看一眼就走」远多于「深读」。
 *
 * # 三个「有则显示、无则隐藏」的区块
 *
 * 子任务（`children`）/ 协作人（`members`）/ 附件（`attachments`）在 T1 的
 * `TaskDetailRes` 里**已经有**，所以直接渲染；空数组时整块不出现，不留白框。
 * 评论在 T1 无接口 → 不渲染（留 TODO）。
 */
import { computed, ref, watch } from 'vue';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import StatusBadge from './components/StatusBadge.vue';
import PriorityTag from './components/PriorityTag.vue';
import { ApiError } from '@/api/request';
import { deleteTask, getTask, type TaskDetail } from '@/api/business/task-api';
import { PERM } from '@/constants/permissions';
import { formatDateTime, formatFileSize } from '@/utils/format';

const props = defineProps<{
  /** 是否可见（v-model） */
  visible: boolean;
  /** 要展示的任务 id */
  taskId?: string | null;
}>();

const emit = defineEmits<{
  /** 可见性变化 */
  (event: 'update:visible', value: boolean): void;
  /** 点「编辑」 */
  (event: 'edit', id: string): void;
  /** 删除成功 */
  (event: 'deleted', id: string): void;
}>();

/** 详情数据 */
const detail = ref<TaskDetail | null>(null);
/** 加载中 */
const loading = ref(false);
/** 错误文案（非空即渲染错误态 + 重试） */
const errorMessage = ref('');

/** 抽屉可见性代理 */
const innerVisible = computed({
  get: () => props.visible,
  set: (value: boolean) => emit('update:visible', value),
});

/** 抽屉标题：有数据用任务号，否则用通用文案 */
const drawerHeader = computed(() => (detail.value ? `任务详情 · ${detail.value.task_no}` : '任务详情'));

/** 是否逾期：有截止日、早于今天、且未完成/未取消 */
const isOverdue = computed(() => {
  const current = detail.value;
  if (!current?.due_date) {
    return false;
  }
  if (current.status === 'DONE' || current.status === 'CANCELLED') {
    return false;
  }
  return dayjs(current.due_date).startOf('day').isBefore(dayjs().startOf('day'));
});

/** 来源文案 */
function sourceLabel(source: string): string {
  const map: Record<string, string> = { SELF: '自建', ASSIGNED: '被分派', TRANSFERRED: '转派' };
  return map[source] ?? source;
}

/** 工时展示：null → 未估 */
function formatHours(hours: number | null): string {
  return hours === null || hours === undefined ? '未估' : `${hours} 小时`;
}

/** 拉详情 */
async function loadDetail(): Promise<void> {
  if (!props.taskId) {
    return;
  }
  loading.value = true;
  errorMessage.value = '';
  try {
    detail.value = await getTask(props.taskId);
  } catch (error) {
    detail.value = null;
    errorMessage.value = error instanceof ApiError ? error.message : '任务详情加载失败，请重试';
  } finally {
    loading.value = false;
  }
}

/** 打开抽屉时加载（同一任务重复打开也重新拉，保证看到最新进度） */
watch(
  () => [props.visible, props.taskId] as const,
  ([visible]) => {
    if (visible) {
      void loadDetail();
    }
  },
  { immediate: true },
);

/** 删除确认 */
function confirmRemove(): void {
  const current = detail.value;
  if (!current) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除任务',
    body: `确认删除「${current.title}」？删除后列表与详情都不再可见（软删除，可由运维恢复）。`,
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteTask(current.id);
        MessagePlugin.success('任务已删除');
        dialog.destroy();
        innerVisible.value = false;
        emit('deleted', current.id);
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败，请稍后重试');
      }
    },
  });
}

/** 关闭后清掉旧数据，避免下次打开先闪上一个任务 */
function handleClosed(): void {
  detail.value = null;
  errorMessage.value = '';
}
</script>

<style scoped lang="less">
.task-detail {
  :deep(.t-drawer__body) {
    padding-top: 8px;
  }

  &__alert {
    margin-bottom: 12px;
  }
}

.head {
  padding-bottom: 16px;
  border-bottom: 1px solid var(--td-component-stroke);

  &__title-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
  }

  &__no {
    font-family: var(--app-font-mono, ui-monospace, monospace);
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__title {
    font-size: 18px;
    font-weight: 600;
    line-height: 1.4;
    color: var(--td-text-color-primary);
    word-break: break-word;
  }

  &__meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 8px;
  }

  &__actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    margin-top: 12px;
  }

  // 禁用按钮组：t-tooltip 需要一个能接收事件的宿主元素，包一层 span
  &__disabled-group {
    display: inline-flex;
    gap: 8px;
    flex-wrap: wrap;
  }
}

.block {
  margin-top: 20px;

  &__title {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
    font-size: 14px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__count {
    padding: 0 6px;
    font-size: 12px;
    font-weight: 400;
    color: var(--td-text-color-placeholder);
    background: var(--td-bg-color-container-hover);
    border-radius: 8px;
  }
}

.progress-row {
  display: flex;
  align-items: center;
  gap: 12px;

  &__hours {
    flex-shrink: 0;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.info {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px 16px;
  margin: 0;

  &__item {
    display: flex;
    gap: 8px;
    font-size: 13px;
    line-height: 22px;
  }

  dt {
    flex-shrink: 0;
    width: 64px;
    color: var(--td-text-color-placeholder);
  }

  dd {
    margin: 0;
    color: var(--td-text-color-primary);
    word-break: break-all;
  }

  &__value--overdue {
    color: var(--td-error-color);
    font-weight: 600;
  }
}

.desc {
  margin: 0;
  padding: 12px;
  font-family: inherit;
  font-size: 13px;
  line-height: 1.7;
  white-space: pre-wrap;
  word-break: break-word;
  color: var(--td-text-color-primary);
  background: var(--td-bg-color-container-hover);
  border-radius: var(--td-radius-medium);
}

.placeholder {
  margin: 0;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
}

.subtask {
  border: 1px solid var(--td-component-stroke);
  border-radius: var(--td-radius-medium);
  overflow: hidden;

  &__item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    font-size: 13px;

    & + & {
      border-top: 1px solid var(--td-component-stroke);
    }
  }

  &__title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--td-text-color-primary);
  }

  &__meta {
    flex-shrink: 0;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.attachment {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 0;
  font-size: 13px;
  color: var(--td-text-color-secondary);

  &__name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__size {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}
</style>
