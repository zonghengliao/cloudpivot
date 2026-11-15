<template>
  <div class="flow-delegate-page">
    <!-- ①② 筛选 / 分页 / 行操作交给 ListPage（P3-12 收口） -->
    <ListPage
      ref="listRef"
      title="审批委托"
      testid="delegate"
      table-testid="delegate-table"
      meta-testid="delegate-meta"
      btn-prefix="delegate"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
    >
      <!--
        视角 / 只看生效中都是「一变就查」（改造前 `@change="handleSearch"`）：
        两个控件走 `custom` 插槽，`@change` 里手动 `search()` 保住这个手感。
      -->
      <template #filter-view>
        <t-select
          v-model="filter.view"
          class="delegate-filter"
          :options="viewOpts"
          data-testid="delegate-view"
          @change="handleSearch"
        />
      </template>

      <template #filter-only_active>
        <t-checkbox v-model="filter.only_active" data-testid="delegate-only-active" @change="handleSearch">
          只看生效中
        </t-checkbox>
      </template>

      <template #toolbar>
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
      </template>

      <!-- 页面说明（原筛选卡下方的 page-tip，文案逐字保留） -->
      <template #filter-footer>
        <p class="page-tip">
          出差 / 休假时把自己的审批权临时让给同事：委托期内新派下来的票直接进受托人的待办，
          待办上标「代你审批」。委托**不需要审批**，但会留痕；同一时间只能委托给一个人。
        </p>
      </template>

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
    </ListPage>

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
import { reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import {
  createFlowDelegate,
  deleteFlowDelegate,
  pageFlowDelegates,
  updateFlowDelegate,
  type FlowDelegate,
  type FlowDelegatePageQuery,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import { PERM } from '@/constants/permissions';
import { FLOW_BIZ_TYPES, FLOW_BIZ_TYPE_LABELS } from '@/api/business/erp/doc-kinds';
import UserSelect from '@/views/business/task/components/UserSelect.vue';

/** 页大小（服务端分页） */
const PAGE_SIZE = 20;

/** 视角选项：我的委托（默认）/ 全部委托（需 business:flow:admin，后端会拦） */
const viewOpts = [
  { label: '我的委托', value: 'mine' },
  { label: '全部委托（管理员）', value: 'all' },
];

/** 筛选状态（`filterState` 就地读写；`queryBuilder` 引用的是同一个对象） */
const filter = reactive<{ view: 'mine' | 'all'; only_active: boolean }>({
  view: 'mine',
  only_active: false,
});

/** ListPage 实例（新增 / 编辑 / 停启用 / 删除后重拉） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/**
 * 筛选项声明。
 *
 * 两项都是 `custom`：视角是普通 select、只看生效中是 checkbox，都不在
 * 声明式控件的四种形态里；「一变就查」由插槽里的 `@change="handleSearch"` 保住。
 */
const filters: FilterDef[] = [
  { key: 'view', type: 'custom', defaultValue: 'mine', width: 220, testid: 'delegate-view' },
  { key: 'only_active', type: 'custom', defaultValue: false, testid: 'delegate-only-active' },
];

/** 表格列 */
const columns: ColumnDef[] = [
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

/** 拉一页 */
const fetcher: ListFetcher<FlowDelegate> = (query) => pageFlowDelegates(query as FlowDelegatePageQuery);

/** 入参拼装（`only_active` 假值就整键缺席 —— 与改造前 `onlyActive || undefined` 一致） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const value = state as { view: 'mine' | 'all'; only_active: boolean };
  return {
    view: value.view,
    only_active: value.only_active || undefined,
    page,
    page_size: pageSize,
  };
};

/** 重拉当前页（新增 / 编辑 / 停启用 / 删除后） */
async function reloadList(): Promise<void> {
  await listRef.value?.reload();
}

/** 查询（回到第 1 页）：筛选控件一变就调它，保住改造前「一变就查」的手感 */
function handleSearch(): void {
  void listRef.value?.search();
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
    await reloadList();
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
    await reloadList();
  } catch (error) {
    MessagePlugin.error(errorMessageOf(error));
  }
}

/** 删除（软删；已派出去的待办不受影响） */
async function removeRow(row: FlowDelegate): Promise<void> {
  try {
    await deleteFlowDelegate(row.id);
    MessagePlugin.success('已删除（历史仍可审计）');
    await reloadList();
  } catch (error) {
    MessagePlugin.error(errorMessageOf(error));
  }
}
</script>

<style scoped>
.flow-delegate-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 视角下拉（custom 筛选插槽自绘） */
.delegate-filter {
  width: 220px;
}

/* 页面说明（#filter-footer）：出差 / 休假代审批的口径 */
.page-tip {
  margin: 12px 0 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.6;
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
