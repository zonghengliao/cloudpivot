<!--
  定时任务（support/job）：任务列表 + 执行日志抽屉（P3-13）。

  # 两张表都进 ListPage
  主表是标准「服务端分页 + 只读行（开关就地启停）」；抽屉里的执行日志同样是服务端分页
  只读列表，所以也交给 ListPage —— 它没有筛选项时整张筛选卡不画（`showFilterCard`），
  抽屉里只出现「标题 + 共 N 条 + 表格 + 分页器」。抽屉的「刷新」挂 `#head-extra`（页头右侧），
  不是为了好看：`ListPage` 的刷新按钮住在筛选卡里，没筛选卡就没地方放。
-->
<template>
  <div class="job-page">
    <ListPage
      ref="listRef"
      title="定时任务"
      testid="job"
      table-testid="job-table"
      meta-testid="job-meta"
      btn-prefix="btn-job"
      :page-size="JOB_PAGE_SIZE"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
    >
      <template #toolbar>
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-job-new"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新增任务
        </t-button>
      </template>

      <template #status="{ row }">
        <t-switch
          :value="row.status === 1"
          :disabled="!hasPermission(PERM.update)"
          :data-testid="`job-status-${row.job_name}`"
          @change="() => toggleStatus(row)"
        />
      </template>
      <template #last_run_at="{ row }">{{ formatDateTime(row.last_run_at) }}</template>
      <template #next_run_at="{ row }">{{ formatDateTime(row.next_run_at) }}</template>

      <template #op="{ row }">
        <t-space size="small">
          <t-link
            v-privilege="[PERM.update]"
            theme="primary"
            data-testid="btn-job-edit"
            @click.stop="openEdit(row)"
          >
            编辑
          </t-link>
          <t-link
            v-privilege="[PERM.trigger]"
            theme="primary"
            data-testid="btn-job-trigger"
            @click.stop="confirmTrigger(row)"
          >
            执行一次
          </t-link>
          <t-link theme="primary" data-testid="btn-job-logs" @click.stop="openLogs(row)">执行日志</t-link>
          <t-link
            v-privilege="[PERM.delete]"
            theme="danger"
            data-testid="btn-job-delete"
            @click.stop="confirmRemove(row)"
          >
            删除
          </t-link>
        </t-space>
      </template>
    </ListPage>

    <t-dialog
      v-model:visible="formVisible"
      :header="isEdit ? '编辑任务' : '新增任务'"
      :confirm-btn="{ content: '确定', theme: 'primary', loading: submitting }"
      width="580px"
      destroy-on-close
      data-testid="job-form-dialog"
      @confirm="submitForm"
    >
      <t-form ref="formRef" :data="form" :rules="formRules" label-width="90px">
        <t-form-item label="任务名称" name="job_name">
          <t-input v-model="form.job_name" placeholder="请输入任务名称" />
        </t-form-item>
        <t-form-item label="任务分组" name="job_group">
          <t-input v-model="form.job_group" placeholder="请输入任务分组" />
        </t-form-item>
        <t-form-item label="cron 表达式" name="cron_expr">
          <t-input v-model="form.cron_expr" placeholder="如 0 0/5 * * * ?" />
        </t-form-item>
        <t-form-item label="调用目标" name="invoke_target">
          <t-input v-model="form.invoke_target" placeholder="请输入调用目标，如 jobs::clean_log" />
        </t-form-item>
        <t-form-item label="调用参数" name="params">
          <t-textarea v-model="form.params" placeholder="请输入调用参数" :autosize="{ minRows: 2 }" />
        </t-form-item>
        <t-form-item label="状态" name="status">
          <t-radio-group v-model="form.status">
            <t-radio :value="1">正常</t-radio>
            <t-radio :value="0">停用</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item label="备注" name="remark">
          <t-textarea v-model="form.remark" placeholder="请输入备注" :maxlength="200" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <t-drawer
      v-model:visible="logsVisible"
      :header="`执行日志 - ${currentRow?.job_name ?? ''}`"
      size="900px"
      :footer="false"
      destroy-on-close
      data-testid="job-logs-drawer"
    >
      <ListPage
        v-if="currentRow"
        ref="logListRef"
        title="执行日志"
        testid="job-log"
        table-testid="job-log-table"
        meta-testid="job-log-meta"
        btn-prefix="btn-job-log"
        :page-size="JOB_PAGE_SIZE"
        :columns="logColumns"
        :fetcher="logFetcher"
        :query-builder="logQueryBuilder"
      >
        <template #head-extra>
          <t-button variant="outline" size="small" data-testid="btn-job-log-refresh" @click="reloadLogs">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </template>
        <template #created_at="{ row }">{{ formatDateTime(row.created_at) }}</template>
        <template #status="{ row }">
          <t-tag :theme="row.status === 1 ? 'success' : 'danger'" variant="light">
            {{ row.status === 1 ? '成功' : '失败' }}
          </t-tag>
        </template>
        <template #op="{ row }">
          <t-link theme="primary" data-testid="btn-job-log-detail" @click.stop="openLogDetail(row)">
            详情
          </t-link>
        </template>
      </ListPage>
    </t-drawer>

    <t-drawer
      v-model:visible="logDetailVisible"
      header="执行日志详情"
      size="560px"
      :footer="false"
      data-testid="job-log-detail-drawer"
    >
      <t-descriptions v-if="currentLog" :column="1" bordered>
        <t-descriptions-item label="任务名称">{{ currentLog.job_name }}</t-descriptions-item>
        <t-descriptions-item label="调用目标">{{ currentLog.invoke_target }}</t-descriptions-item>
        <t-descriptions-item label="执行结果">
          <t-tag :theme="currentLog.status === 1 ? 'success' : 'danger'" variant="light">
            {{ currentLog.status === 1 ? '成功' : '失败' }}
          </t-tag>
        </t-descriptions-item>
        <t-descriptions-item label="开始时间">{{ formatDateTime(currentLog.started_at) }}</t-descriptions-item>
        <t-descriptions-item label="结束时间">{{ formatDateTime(currentLog.finished_at) }}</t-descriptions-item>
        <t-descriptions-item label="耗时">{{ currentLog.duration_ms }} ms</t-descriptions-item>
        <t-descriptions-item label="执行消息">{{ currentLog.message || '-' }}</t-descriptions-item>
        <t-descriptions-item label="异常信息">
          <pre class="job-page__detail">{{ currentLog.error || '-' }}</pre>
        </t-descriptions-item>
      </t-descriptions>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { reactive, ref } from 'vue';
import {
  DialogPlugin,
  MessagePlugin,
  type FormInstanceFunctions,
  type FormRules,
} from 'tdesign-vue-next';
import {
  changeJobStatus,
  createJob,
  deleteJob,
  getJob,
  pageJobs,
  pageJobLogs,
  triggerJob,
  updateJob,
  type JobLogRes,
  type JobRes,
} from '@/api/support/job-api';
import type { StatusValue } from '@/api/system/user-api';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import { formatDateTime } from '@/utils/format';
import ListPage from '@/components/ListPage/index.vue';
import type {
  ColumnDef,
  FilterDef,
  ListFetcher,
  ListQuery,
  ListQueryBuilder,
} from '@/components/ListPage/types';

/**
 * 本页权限码（与 `sys_menu.auth_code` 一致）。
 *
 * 就近放在页面里（不改 `src/constants/permissions.ts`）：那个文件是全站单点，
 * 多张卡会同时往它里面加码，谁后提交谁撞冲突。
 */
const PERM = {
  create: 'support:job:create',
  update: 'support:job:update',
  trigger: 'support:job:trigger',
  delete: 'support:job:delete',
} as const;

/** 每页条数（改造前是 10，保持手感一致） */
const JOB_PAGE_SIZE = 10;

/** 查询条件（`''` / `undefined` = 不过滤） */
interface JobFilterState {
  /** 任务名称 */
  job_name: string;
  /** 任务分组 */
  job_group: string;
  /** 状态：1 正常 / 0 停用 / '' 全部 */
  status: StatusValue | '';
}

/** 新增/编辑表单 */
interface JobForm {
  /** 任务 id，新增时为空 */
  id: string;
  /** 任务名称 */
  job_name: string;
  /** 任务分组 */
  job_group: string;
  /** cron 表达式 */
  cron_expr: string;
  /** 调用目标 */
  invoke_target: string;
  /** 调用参数 */
  params: string;
  /** 状态 */
  status: StatusValue;
  /** 备注 */
  remark: string;
}

const userStore = useUserStore();
const { hasPermission } = userStore;

const filter = reactive<JobFilterState>({ job_name: '', job_group: '', status: '' });
/** ListPage 实例（保存 / 启停 / 删除后重拉当前页） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

const columns: ColumnDef[] = [
  { colKey: 'job_name', title: '任务名称', width: 180 },
  { colKey: 'job_group', title: '分组', width: 120 },
  { colKey: 'cron_expr', title: 'cron 表达式', width: 170 },
  { colKey: 'invoke_target', title: '调用目标', ellipsis: true },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'last_run_at', title: '上次执行', width: 180 },
  { colKey: 'next_run_at', title: '下次执行', width: 180 },
  { colKey: 'op', title: '操作', width: 260, fixed: 'right', exportable: false },
];

/** 执行日志列（抽屉里那张表） */
const logColumns: ColumnDef[] = [
  { colKey: 'created_at', title: '记录时间', width: 180 },
  { colKey: 'invoke_target', title: '调用目标', width: 200, ellipsis: true },
  { colKey: 'status', title: '结果', width: 90 },
  { colKey: 'duration_ms', title: '耗时(ms)', width: 100 },
  { colKey: 'message', title: '消息', ellipsis: true },
  { colKey: 'op', title: '操作', width: 80, fixed: 'right', exportable: false },
];

/** 筛选行：两个关键词 + 状态下拉（改造前都是点「查询」才生效） */
const filters: FilterDef[] = [
  { key: 'job_name', type: 'keyword', placeholder: '任务名称', width: 220, testid: 'job-filter-name' },
  { key: 'job_group', type: 'keyword', placeholder: '任务分组', width: 220, testid: 'job-filter-group' },
  {
    key: 'status',
    type: 'select',
    placeholder: '状态',
    width: 130,
    options: [
      { label: '正常', value: 1 },
      { label: '停用', value: 0 },
    ],
    testid: 'job-filter-status',
  },
];

/** 拉一页任务 */
const fetcher: ListFetcher<JobRes> = (query) => pageJobs(query as Parameters<typeof pageJobs>[0]);

/**
 * 任务列表入参。
 *
 * 键名与改造前逐字一致（`job_name` / `job_group` / `status`）；空值整键缺席，
 * `status = 0`（停用）是**真条件**，必须发出去 —— 用 `if (status)` 会把它丢掉。
 */
const queryBuilder: ListQueryBuilder = (state, page, pageSize): ListQuery => {
  const { job_name: name, job_group: group, status } = state as JobFilterState;
  const keyword = String(name ?? '').trim();
  const groupText = String(group ?? '').trim();
  return {
    page,
    page_size: pageSize,
    ...(keyword ? { job_name: keyword } : {}),
    ...(groupText ? { job_group: groupText } : {}),
    ...(status === 1 || status === 0 ? { status } : {}),
  };
};

// ==================== 执行日志（抽屉） ====================

/** 当前任务（抽屉表头 + 日志查询的 `job_id`） */
const currentRow = ref<JobRes | null>(null);
const logsVisible = ref(false);
const logListRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 拉一页执行日志（`job_id` 由抽屉选中的任务决定） */
const logFetcher: ListFetcher<JobLogRes> = (query) =>
  pageJobLogs({
    ...(query as { page: number; page_size: number }),
    job_id: currentRow.value?.id ?? '',
  });

const logQueryBuilder: ListQueryBuilder = (_state, page, pageSize) => ({ page, page_size: pageSize });

/** 抽屉里的「刷新」（列表卡页头右侧） */
async function reloadLogs(): Promise<void> {
  await logListRef.value?.reload();
}

function openLogs(row: JobRes): void {
  currentRow.value = row;
  logsVisible.value = true;
}

// ==================== 表单 ====================

const formVisible = ref(false);
const isEdit = ref(false);
const submitting = ref(false);
const formRef = ref<FormInstanceFunctions | null>(null);
const form = reactive<JobForm>({
  id: '',
  job_name: '',
  job_group: '',
  cron_expr: '',
  invoke_target: '',
  params: '',
  status: 1,
  remark: '',
});
const formRules: FormRules<JobForm> = {
  job_name: [{ required: true, message: '请输入任务名称' }],
  cron_expr: [{ required: true, message: '请输入 cron 表达式' }],
  invoke_target: [{ required: true, message: '请输入调用目标' }],
};

const logDetailVisible = ref(false);
const currentLog = ref<JobLogRes | null>(null);

/** 统一展示接口错误 */
function showError(message: string): void {
  void MessagePlugin.error(message);
}

/** 重拉当前页（保存 / 启停 / 删除后） */
async function reloadList(): Promise<void> {
  await listRef.value?.reload();
}

function openCreate(): void {
  isEdit.value = false;
  Object.assign(form, {
    id: '',
    job_name: '',
    job_group: '',
    cron_expr: '',
    invoke_target: '',
    params: '',
    status: 1,
    remark: '',
  });
  formVisible.value = true;
}

async function openEdit(row: JobRes): Promise<void> {
  try {
    const detail = await getJob(row.id);
    isEdit.value = true;
    Object.assign(form, {
      id: detail.id,
      job_name: detail.job_name,
      job_group: detail.job_group,
      cron_expr: detail.cron_expr,
      invoke_target: detail.invoke_target,
      params: detail.params,
      status: detail.status,
      remark: detail.remark,
    });
    formVisible.value = true;
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '任务详情加载失败');
  }
}

async function submitForm(): Promise<void> {
  const valid = await formRef.value?.validate();
  if (valid !== true) {
    return;
  }
  const payload = {
    job_name: form.job_name,
    job_group: form.job_group,
    cron_expr: form.cron_expr,
    invoke_target: form.invoke_target,
    params: form.params,
    status: form.status,
    remark: form.remark,
  };
  submitting.value = true;
  try {
    if (isEdit.value) {
      await updateJob(form.id, payload);
      void MessagePlugin.success('任务已更新');
    } else {
      await createJob(payload);
      void MessagePlugin.success('任务已创建');
    }
    formVisible.value = false;
    await reloadList();
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '保存失败');
  } finally {
    submitting.value = false;
  }
}

/** 启停任务 */
function toggleStatus(row: JobRes): void {
  const next: StatusValue = row.status === 1 ? 0 : 1;
  const action = next === 1 ? '启用' : '停用';
  const dialog = DialogPlugin.confirm({
    header: `${action}任务`,
    body: `确认${action}任务「${row.job_name}」？`,
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await changeJobStatus(row.id, next);
          void MessagePlugin.success(`已${action}`);
          await reloadList();
        } catch (error) {
          showError(error instanceof ApiError ? error.message : `${action}失败`);
        }
      })();
    },
  });
}

/** 手动执行一次任务 */
function confirmTrigger(row: JobRes): void {
  const dialog = DialogPlugin.confirm({
    header: '手动执行',
    body: `确认立即执行任务「${row.job_name}」一次？`,
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await triggerJob(row.id);
          void MessagePlugin.success('已触发执行');
        } catch (error) {
          showError(error instanceof ApiError ? error.message : '触发执行失败');
        }
      })();
    },
  });
}

function confirmRemove(row: JobRes): void {
  const dialog = DialogPlugin.confirm({
    header: '删除任务',
    body: `确认删除任务「${row.job_name}」？该操作不可恢复。`,
    theme: 'danger',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await deleteJob(row.id);
          void MessagePlugin.success('任务已删除');
          await reloadList();
        } catch (error) {
          showError(error instanceof ApiError ? error.message : '删除失败');
        }
      })();
    },
  });
}

function openLogDetail(row: JobLogRes): void {
  currentLog.value = row;
  logDetailVisible.value = true;
}
</script>

<style scoped lang="less">
.job-page {
  padding: 16px;

  &__detail {
    margin: 0;
    word-break: break-all;
    white-space: pre-wrap;
  }
}
</style>
