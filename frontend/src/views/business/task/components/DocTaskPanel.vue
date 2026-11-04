<template>
  <div class="doc-tasks" :data-testid="testid">
    <div class="doc-tasks__head">
      <span class="doc-tasks__title">关联任务</span>
      <span class="doc-tasks__meta" :data-testid="`${testid}-count`">
        共 {{ rows.length }} 条<template v-if="total > rows.length">（只显示前 {{ rows.length }} 条）</template>
      </span>
      <span class="doc-tasks__spacer" />
      <t-link theme="primary" :data-testid="`${testid}-open-list`" @click="openList">去任务列表</t-link>
    </div>

    <t-alert v-if="error" theme="error" :message="error" class="doc-tasks__alert">
      <template #operation>
        <t-button size="small" variant="outline" :data-testid="`${testid}-retry`" @click="load">重试</t-button>
      </template>
    </t-alert>

    <div v-if="loading" class="doc-tasks__loading"><t-loading size="small" /> 加载中…</div>
    <p v-else-if="rows.length === 0" class="doc-tasks__empty" :data-testid="`${testid}-empty`">
      还没有挂在这张单上的任务 —— 下面直接建一条（会带上单据关联，任务详情里能点回本单）。
    </p>
    <ul v-else class="doc-tasks__list">
      <li v-for="task in rows" :key="task.id" class="doc-tasks__row" :data-testid="`${testid}-row-${task.task_no}`">
        <span class="doc-tasks__no mono">{{ task.task_no }}</span>
        <t-link theme="primary" class="doc-tasks__name" @click="openTask(task.id)">{{ task.title }}</t-link>
        <StatusBadge domain="task" :status="task.status" />
        <span class="doc-tasks__owner">{{ task.owner_name }}</span>
        <span class="doc-tasks__due muted" :class="{ 'is-overdue': overdue(task) }">
          {{ task.due_date ?? '无期限' }}
        </span>
      </li>
    </ul>

    <div class="doc-tasks__create">
      <t-input
        v-model="draftTitle"
        placeholder="新建跟进任务，如：催货 / 跟进到货 / 对账"
        :maxlength="200"
        :data-testid="`${testid}-title`"
        @enter="submit"
      />
      <t-button
        theme="primary"
        variant="outline"
        :loading="creating"
        :disabled="!draftTitle.trim()"
        :data-testid="`${testid}-create`"
        @click="submit"
      >
        <template #icon><t-icon name="add" /></template>
        新建任务
      </t-button>
    </div>
    <p class="doc-tasks__hint muted">
      新建的任务自动挂到本单（`doc_type` / `doc_id` 由本组件带上）；单据侧的列表只显示**我有权看见**的任务。
    </p>
  </div>
</template>

<script setup lang="ts">
/**
 * 单据详情页底部的「关联任务」区（P2-9 / DC-19）。
 *
 * # 它解决什么
 *
 * 业务单据与工作任务原本只能靠「标题里手抄单号」关联：查不到、点不回、单据侧
 * 也列不出自己身上的任务。这个面板把两件事一次做完：
 * 1. 列出挂在本单上的任务（后端 `GET /business/task/by-doc`，**已按任务可见性
 *    过滤** —— 挂单不是共享权限，别人挂在同一张单上的任务我看不到）；
 * 2. 直接新建一条带 `doc_type` / `doc_id` 的跟进任务（不用去任务页再手工挂）。
 *
 * # 为什么不在采购页各写一遍
 *
 * 以后收货单 / 合同 / 应付 / 凭证都要这个区块（后端白名单已经是 5 种单据），
 * 所以抽成一个组件：调用方只传 `doc-type` / `doc-id`，不再重复实现「查询 + 新建」。
 *
 * # 边界（本卡不做）
 *
 * **不合并**两套任务系统：这里列的是 `biz_task`（工作任务），审批待办仍在
 * `biz_flow_task`（流程引擎自己管）。二者各司其职，本面板只加一条边。
 */
import { ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';

import { createTask, getTasksByDoc, type TaskCard } from '@/api/business/task-api';

import StatusBadge from './StatusBadge.vue';

const props = withDefaults(
  defineProps<{
    /** 单据类型（后端白名单：`PURCHASE_ORDER` / `STOCK_IN` / `CONTRACT` / `PAYABLE` / `VOUCHER`） */
    docType: string;
    /** 单据主键（弱引用；后端会校验「存在且未软删」） */
    docId: string;
    /** 单据单号（仅用于「去任务列表」时定位；可空） */
    docNo?: string | null;
    /** 测试锚点前缀（列表页 / 详情页可各给一个，避免同名冲突） */
    testid?: string;
  }>(),
  { docNo: null, testid: 'doc-tasks' },
);

const router = useRouter();

const rows = ref<TaskCard[]>([]);
const total = ref(0);
const loading = ref(false);
const error = ref('');
const creating = ref(false);
const draftTitle = ref('');

/** 逾期判定：只有「未终态 + 有截止日 + 已过期」才算（与任务列表口径一致）。 */
function overdue(task: TaskCard): boolean {
  if (!task.due_date || task.status === 'DONE' || task.status === 'CANCELLED') {
    return false;
  }
  const today = new Date();
  const cst = new Date(today.getTime() + 8 * 3600 * 1000).toISOString().slice(0, 10);
  return task.due_date < cst;
}

/** 拉取本单的关联任务（失败只影响本区块，不打断单据详情）。 */
async function load(): Promise<void> {
  if (!props.docId) {
    rows.value = [];
    total.value = 0;
    return;
  }
  loading.value = true;
  error.value = '';
  try {
    const res = await getTasksByDoc(props.docType, props.docId);
    rows.value = res.list ?? [];
    total.value = res.total ?? rows.value.length;
  } catch (err) {
    error.value = err instanceof Error ? err.message : '关联任务加载失败';
  } finally {
    loading.value = false;
  }
}

/** 建一条带单据关联的跟进任务（负责人 = 当前登录人，交后端默认）。 */
async function submit(): Promise<void> {
  const title = draftTitle.value.trim();
  if (!title || creating.value) {
    return;
  }
  creating.value = true;
  try {
    await createTask({ title, doc_type: props.docType, doc_id: props.docId });
    draftTitle.value = '';
    MessagePlugin.success('任务已创建，并挂到本单');
    await load();
  } catch (err) {
    MessagePlugin.error(err instanceof Error ? err.message : '新建任务失败');
  } finally {
    creating.value = false;
  }
}

/** 打开某条任务：任务详情是列表页的抽屉（`/task/list?id=<id>`），不是独立路由。 */
function openTask(id: string): void {
  router.push({ path: '/task/list', query: { id } });
}

/** 去任务列表（不带 id）：用户要自己筛 / 批量看全部任务时用。 */
function openList(): void {
  router.push({ path: '/task/list' });
}

// 单据切换（详情抽屉在列表上复用，`docId` 会变）→ 重新拉一次
watch(() => [props.docType, props.docId], load, { immediate: true });
</script>

<style scoped lang="less">
.doc-tasks {
  margin-top: 16px;
  padding: 12px 14px;
  border: 1px solid var(--td-component-border);
  border-radius: 6px;

  &__head {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
  }

  &__title {
    font-weight: 600;
  }

  &__meta,
  &__hint {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__spacer {
    flex: 1;
  }

  &__alert {
    margin-bottom: 8px;
  }

  &__loading {
    padding: 8px 0;
    color: var(--td-text-color-secondary);
  }

  &__empty {
    margin: 0 0 8px;
    color: var(--td-text-color-secondary);
    font-size: 13px;
  }

  &__list {
    margin: 0 0 8px;
    padding: 0;
    list-style: none;
  }

  &__row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 0;
    border-bottom: 1px dashed var(--td-component-stroke);
    font-size: 13px;

    &:last-child {
      border-bottom: none;
    }
  }

  &__no {
    width: 148px;
    color: var(--td-text-color-secondary);
  }

  &__name {
    flex: 1;
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__owner {
    width: 72px;
  }

  &__due {
    width: 92px;
    text-align: right;

    &.is-overdue {
      color: var(--td-error-color);
    }
  }

  &__create {
    display: flex;
    gap: 8px;
  }

  &__hint {
    margin: 6px 0 0;
  }
}
</style>
