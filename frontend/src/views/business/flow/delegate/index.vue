<template>
  <div class="flow-delegate-page">
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="[16, 12]">
        <t-col :span="5">
          <t-select
            v-model="view"
            :options="viewOpts"
            data-testid="delegate-view"
            @change="handleSearch"
          />
        </t-col>
        <t-col :span="4">
          <t-checkbox v-model="onlyActive" data-testid="delegate-only-active" @change="handleSearch">
            只看生效中
          </t-checkbox>
        </t-col>
        <t-col :span="15">
          <t-space>
            <t-button theme="primary" data-testid="delegate-search" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button
              v-privilege="[PERM.flowDelegateSave]"
              theme="primary"
              variant="outline"
              data-testid="delegate-create"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新增委托
            </t-button>
            <t-button variant="outline" :loading="loading" @click="loadList">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>
          </t-space>
        </t-col>
      </t-row>
      <p class="page-tip">
        出差 / 休假时把自己的审批权临时让给同事：委托期内新派下来的票直接进受托人的待办，
        待办上标「代你审批」。委托**不需要审批**，但会留痕；同一时间只能委托给一个人。
      </p>
    </t-card>

    <t-card :bordered="false">
      <t-alert v-if="errorText" class="list-alert" theme="error" :message="errorText">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadList">重试</t-button>
        </template>
      </t-alert>
      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        data-testid="delegate-table"
        @page-change="onPageChange"
      >
        <template #delegator="{ row }">
          <span>{{ row.delegator_name || '—' }}</span>
        </template>
        <template #delegate="{ row }">
          <span>{{ row.delegate_name || '（用户已删除）' }}</span>
        </template>
        <template #period="{ row }">
          <span class="period">
            {{ formatTime(row.start_at) }} → {{ formatTime(row.end_at) }}
          </span>
        </template>
        <template #scope="{ row }">
          <span>{{ scopeLabel(row.scope) }}</span>
        </template>
        <template #state="{ row }">
          <t-tag v-if="!row.enabled" theme="default" variant="light" size="small">已停用</t-tag>
          <t-tag v-else-if="row.is_active_now" theme="success" variant="light" size="small">
            生效中
          </t-tag>
          <t-tag v-else-if="new Date(row.start_at).getTime() > Date.now()" theme="primary" variant="light" size="small">
            未开始
          </t-tag>
          <t-tag v-else theme="default" variant="light" size="small">已过期</t-tag>
        </template>
        <template #ops="{ row }">
          <t-space size="small">
            <t-link v-privilege="[PERM.flowDelegateSave]" theme="primary" @click="openEdit(row)">
              编辑
            </t-link>
            <t-link v-privilege="[PERM.flowDelegateSave]" theme="warning" @click="toggleEnabled(row)">
              {{ row.enabled ? '停用' : '启用' }}
            </t-link>
            <t-link v-privilege="[PERM.flowDelegateDelete]" theme="danger" @click="removeRow(row)">
              删除
            </t-link>
          </t-space>
        </template>
      </t-table>
    </t-card>

    <t-dialog
      v-model:visible="dialogVisible"
      :header="form.id ? '编辑委托' : '新增委托'"
      width="560px"
      :confirm-btn="{ content: '保存', theme: 'primary', loading: saving }"
      data-testid="delegate-dialog"
      @confirm="submit"
    >
      <t-form label-align="top">
        <t-form-item label="受托人" required>
          <UserSelect v-model="form.delegate_id" placeholder="搜索用户名 / 姓名（代你审批的人）" />
        </t-form-item>
        <t-form-item label="生效时间" required>
          <t-date-range-picker
            v-model="form.period"
            enable-time-picker
            format="YYYY-MM-DD HH:mm"
            :clearable="false"
            style="width: 100%"
            data-testid="delegate-period"
          />
        </t-form-item>
        <t-form-item label="生效范围">
          <t-select
            v-model="form.scope"
            clearable
            placeholder="留空 = 全部业务"
            :options="scopeOpts"
            data-testid="delegate-scope"
          />
        </t-form-item>
        <t-form-item label="事由">
          <t-input v-model="form.reason" placeholder="出差 / 休假 …（可选，最多 200 字）" :maxlength="200" />
        </t-form-item>
        <t-form-item v-if="form.id" label="启用">
          <t-switch v-model="form.enabled" />
        </t-form-item>
        <p class="form-tip">
          受托人不能是发起人 / 制单人（内控不允许自审）；与你已有的委托区间不能重叠。
        </p>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 审批委托（P3-04 / ARCH-8 场景 E · `/flow/delegate`）。
 *
 * # 一句话
 *
 * 出差 / 休假时把自己的审批权临时让给同事 —— 委托期内**新派下来**的票直接进受托人的待办，
 * 待办上标「代你审批」（后端写 `assigned_by_rule='DELEGATED'` + `comment`）。
 *
 * # 四个接口（全部真接口，失败就渲染错误态，前端不造数据）
 *
 * | 动作 | 接口 | 权限码 |
 * |------|------|--------|
 * | 列表 | `GET /business/flow/delegate/page` | `business:flow:delegate:list` |
 * | 新增 | `POST /business/flow/delegate` | `business:flow:delegate:save` |
 * | 编辑 / 停用 | `PUT /business/flow/delegate/{id}` | `business:flow:delegate:save` |
 * | 删除 | `DELETE /business/flow/delegate/{id}` | `business:flow:delegate:delete` |
 *
 * 行级范围在后端：只有本人（或 `business:flow:admin`）能改 / 删自己的委托 ——
 * 前端按钮隐身只是体验，不是权限。
 *
 * # 错误码文案
 *
 * `422 + 65043`（区间非法 / 自委托 / 区间重叠）直接把后端 `msg` 弹出来 ——
 * 那段文案已经写了「跟哪条冲突、怎么改」，前端不该再编一遍。
 */
import { onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import {
  createFlowDelegate,
  deleteFlowDelegate,
  pageFlowDelegates,
  updateFlowDelegate,
  type FlowDelegate,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import { FLOW_BIZ_TYPES, FLOW_BIZ_TYPE_LABELS } from '@/api/business/erp/doc-kinds';
import UserSelect from '@/views/business/task/components/UserSelect.vue';

/** 视角：我的委托（默认）/ 全部委托（需 business:flow:admin，后端会拦） */
const view = ref<'mine' | 'all'>('mine');
const viewOpts = [
  { label: '我的委托', value: 'mine' },
  { label: '全部委托（管理员）', value: 'all' },
];
/** 只看生效中 */
const onlyActive = ref(false);
/** 当前页数据 */
const rows = ref<FlowDelegate[]>([]);
/** 总数 */
const total = ref(0);
/** 页码 */
const page = ref(1);
/** 页大小 */
const pageSize = ref(20);
/** 加载中 */
const loading = ref(false);
/** 列表错误（真接口失败时显示，不静默空表） */
const errorText = ref('');

/** 分页配置 */
const pagination = reactive({
  current: 1,
  pageSize: 20,
  total: 0,
  showJumper: true,
});

/** 表格列 */
const columns: PrimaryTableCol[] = [
  { colKey: 'delegator', title: '委托人', width: 140 },
  { colKey: 'delegate', title: '受托人', width: 140 },
  { colKey: 'period', title: '生效时间', width: 300 },
  { colKey: 'scope', title: '生效范围', width: 160 },
  { colKey: 'reason', title: '事由', ellipsis: true },
  { colKey: 'state', title: '状态', width: 100 },
  { colKey: 'ops', title: '操作', width: 180 },
];

/** 生效范围下拉（业务类型词表来自 doc-kinds.ts 的唯一真相，不在这里写字面量） */
const scopeOpts = FLOW_BIZ_TYPES.map((value) => ({
  label: `${FLOW_BIZ_TYPE_LABELS[value]}（${value}）`,
  value,
}));

/** 生效范围显示：空串 = 全部业务 */
function scopeLabel(scope: string): string {
  if (!scope) {
    return '全部业务';
  }
  const label = (FLOW_BIZ_TYPE_LABELS as Record<string, string | undefined>)[scope];
  return label ? `${label}（${scope}）` : scope;
}

/** RFC3339 → `YYYY-MM-DD HH:mm`（本地时区；后端存 UTC，展示给人看的是业务时区） */
function formatTime(iso: string): string {
  const date = new Date(iso);
  if (!Number.isFinite(date.getTime())) {
    return '—';
  }
  const pad = (value: number): string => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

/** 后端返回的错误文案（`ApiError` 里 `message` 已经是后端中文 `msg`） */
function errorMessageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 拉列表 */
async function loadList(): Promise<void> {
  loading.value = true;
  errorText.value = '';
  try {
    const data = await pageFlowDelegates({
      view: view.value,
      only_active: onlyActive.value || undefined,
      page: page.value,
      page_size: pageSize.value,
    });
    rows.value = data.list;
    total.value = data.total;
    pagination.total = data.total;
    pagination.current = data.page;
    pagination.pageSize = data.page_size;
  } catch (error) {
    rows.value = [];
    total.value = 0;
    errorText.value = `加载委托失败：${errorMessageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

/** 查询（回到第 1 页） */
function handleSearch(): void {
  page.value = 1;
  pagination.current = 1;
  void loadList();
}

/** 翻页 */
function onPageChange(info: { current: number; pageSize: number }): void {
  page.value = info.current;
  pageSize.value = info.pageSize;
  void loadList();
}

// ==================== 表单 ====================

/** 弹窗可见 */
const dialogVisible = ref(false);
/** 保存中 */
const saving = ref(false);
/** 表单（`period` 是 `[start, end]`，字符串形态 `YYYY-MM-DD HH:mm`，后端三种形态都吃） */
const form = reactive({
  id: '',
  delegate_id: '',
  period: [] as string[],
  scope: '' as string,
  reason: '',
  enabled: true,
});

/** 打开「新增」 */
function openCreate(): void {
  form.id = '';
  form.delegate_id = '';
  form.period = [];
  form.scope = '';
  form.reason = '';
  form.enabled = true;
  dialogVisible.value = true;
}

/** 打开「编辑」（把 RFC3339 转成选择器要的字符串） */
function openEdit(row: FlowDelegate): void {
  form.id = row.id;
  form.delegate_id = row.delegate_id;
  form.period = [formatTime(row.start_at), formatTime(row.end_at)];
  form.scope = row.scope || '';
  form.reason = row.reason || '';
  form.enabled = row.enabled;
  dialogVisible.value = true;
}

/** 提交（新建 / 全量覆盖修改） */
async function submit(): Promise<void> {
  if (!form.delegate_id) {
    MessagePlugin.warning('请选择受托人');
    return;
  }
  if (form.period.length !== 2 || !form.period[0] || !form.period[1]) {
    MessagePlugin.warning('请选择生效时间范围');
    return;
  }
  saving.value = true;
  try {
    const body = {
      delegate_id: form.delegate_id,
      start_at: form.period[0],
      end_at: form.period[1],
      scope: form.scope || undefined,
      reason: form.reason || undefined,
      enabled: form.enabled,
    };
    if (form.id) {
      await updateFlowDelegate(form.id, body);
      MessagePlugin.success('委托已更新');
    } else {
      await createFlowDelegate(body);
      MessagePlugin.success('委托已创建，生效后新派下来的审批会直接进受托人的待办');
    }
    dialogVisible.value = false;
    await loadList();
  } catch (error) {
    // 65043（区间非法 / 自委托 / 重叠）等业务错误的文案直接来自后端
    MessagePlugin.error(errorMessageOf(error));
  } finally {
    saving.value = false;
  }
}

/** 停用 / 启用（保留区间与历史，只切开关） */
async function toggleEnabled(row: FlowDelegate): Promise<void> {
  try {
    await updateFlowDelegate(row.id, {
      delegate_id: row.delegate_id,
      start_at: row.start_at,
      end_at: row.end_at,
      scope: row.scope || undefined,
      reason: row.reason || undefined,
      enabled: !row.enabled,
    });
    MessagePlugin.success(row.enabled ? '已停用' : '已启用');
    await loadList();
  } catch (error) {
    MessagePlugin.error(errorMessageOf(error));
  }
}

/** 删除（软删；已派出去的待办不受影响） */
async function removeRow(row: FlowDelegate): Promise<void> {
  try {
    await deleteFlowDelegate(row.id);
    MessagePlugin.success('已删除（历史仍可审计）');
    await loadList();
  } catch (error) {
    MessagePlugin.error(errorMessageOf(error));
  }
}

onMounted(() => {
  void loadList();
});
</script>

<style scoped>
.flow-delegate-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card {
  padding: 4px 0;
}

.page-tip {
  margin: 12px 0 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.6;
}

.list-alert {
  margin-bottom: 12px;
}

.period {
  font-variant-numeric: tabular-nums;
}

.form-tip {
  margin: 4px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 1.6;
}
</style>
