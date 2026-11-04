<!--
  预算科目映射页（FIN-1-FE / ARCH-6 §3.6）。

  采购单没有科目列 —— 一张采购单要落到哪个**预算科目**槽，全靠这张映射表：
  `MATERIAL`（单个物料）> `WAREHOUSE`（仓库）> `MATERIAL_TYPE`（物料类型）> `DEFAULT`（全公司兜底）。

  # 两个必须照实说的坑（`budget-logic.ts` 里也有同样注释）

  1. 列表的「类型」筛选参数名后端叫 **`status`**（`budget_model::map_page` 复用了 `BudgetFilter`），
     页面走 `fetchBudgetMappingPage` 的语义化 `map_type`，翻译在 API 层做，别在这里改回去；
  2. `MATERIAL_TYPE` 这一版**存不进去**：校验侧要求 `ref_id`、解析侧只认 `ref_code`。
     表单里直接拦住并写明原因（`mappingSaveBlocker`），不让用户填半天吃 400。
-->
<template>
  <div class="mapping-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM]"
          theme="primary"
          data-testid="budget-mapping-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建映射
        </t-button>
        <t-button variant="outline" data-testid="budget-mapping-btn-refresh" @click="loadRows">刷新</t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.map_type"
          class="tool-row__type"
          :options="mapTypeFilterOptions"
          placeholder="全部类型"
          clearable
          data-testid="budget-mapping-filter-type"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.dept_id"
          class="tool-row__dept"
          :options="deptOptions"
          placeholder="全部部门"
          clearable
          filterable
          data-testid="budget-mapping-filter-dept"
          @change="handleSearch"
        />
        <t-button theme="primary" data-testid="budget-mapping-btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="budget-mapping-btn-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 映射清单 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">预算科目映射</span>
          <span class="panel-head__meta" data-testid="budget-mapping-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">
          命中顺序：物料 &gt; 仓库 / 物料类型（比优先级）&gt; 全公司兜底；一条都不命中则不占用预算
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="budget-mapping-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadRows">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="budget-mapping-table"
        @page-change="handlePageChange"
      >
        <template #map_type="{ row }">
          <t-tag size="small" variant="light" :theme="mapTypeTheme(row.map_type)">
            {{ mapTypeLabel(row.map_type) }}
          </t-tag>
        </template>
        <template #target="{ row }">
          <span class="mono">{{ mappingTargetText(row) }}</span>
        </template>
        <template #account="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.account_name }}</span>
            <span class="name-cell__sub">{{ row.account_code }}</span>
          </div>
        </template>
        <template #dept_id="{ row }">
          <span class="muted">{{ deptNameOf(row.dept_id) }}</span>
        </template>
        <template #status="{ row }">
          <t-tag size="small" variant="light" :theme="mappingStatusTheme(row.status)">
            {{ mappingStatusLabel(row.status) }}
          </t-tag>
        </template>
        <template #remark="{ row }">
          <span class="muted">{{ dash(row.remark) }}</span>
        </template>
        <template #op="{ row }">
          <div class="op-cell">
            <t-link
              v-privilege="[PERM]"
              theme="primary"
              :data-testid="`budget-mapping-btn-edit-${row.id}`"
              @click="openEdit(row)"
            >
              编辑
            </t-link>
            <t-link
              v-privilege="[PERM]"
              theme="danger"
              :data-testid="`budget-mapping-btn-delete-${row.id}`"
              @click="confirmDelete(row)"
            >
              删除
            </t-link>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑映射 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑映射' : '新建映射'"
      width="640px"
      :confirm-btn="{ content: '保存', loading: submitting, disabled: !!saveBlocker }"
      :cancel-btn="{ content: '取消' }"
      :close-on-overlay-click="!submitting"
      data-testid="budget-mapping-form-dialog"
      @confirm="submitForm"
      @close="closeForm"
    >
      <t-form :data="draft" label-width="104px" colon>
        <t-form-item label="映射类型" :help="mapTypeHint(draft.map_type)">
          <t-select
            v-model="draft.map_type"
            :options="mapTypeOptions"
            data-testid="budget-mapping-form-type"
            @change="handleTypeChange"
          />
        </t-form-item>

        <t-form-item v-if="draft.map_type === 'DEFAULT'" label="映射目标">
          <t-tag size="small" variant="light">全公司兜底（不指向任何目标）</t-tag>
        </t-form-item>

        <t-form-item v-else-if="draft.map_type === 'MATERIAL_TYPE'" label="物料类型">
          <t-select
            v-model="draft.ref_code"
            :options="materialTypeOptions"
            placeholder="选择物料类型（解析只认编码）"
            data-testid="budget-mapping-form-material-type"
            @change="handleMaterialTypeChange"
          />
        </t-form-item>

        <t-form-item v-else-if="draft.map_type === 'MATERIAL'" label="物料">
          <t-select
            v-model="draft.ref_id"
            :options="materialOptions"
            placeholder="搜索物料编码 / 名称"
            filterable
            :loading="optionLoading"
            data-testid="budget-mapping-form-material"
            @change="handleMaterialChange"
          />
        </t-form-item>

        <t-form-item v-else label="仓库">
          <t-select
            v-model="draft.ref_id"
            :options="warehouseOptions"
            placeholder="选择仓库"
            filterable
            :loading="optionLoading"
            data-testid="budget-mapping-form-warehouse"
            @change="handleWarehouseChange"
          />
        </t-form-item>

        <t-form-item label="预算科目">
          <t-select
            v-model="draft.account_id"
            :options="accountOptions"
            placeholder="选择末级预算科目"
            filterable
            data-testid="budget-mapping-form-account"
          />
        </t-form-item>

        <t-form-item label="部门覆盖">
          <t-select
            v-model="draft.dept_id"
            :options="deptOptions"
            placeholder="留空 = 全公司"
            clearable
            filterable
            data-testid="budget-mapping-form-dept"
          />
        </t-form-item>

        <t-form-item label="优先级" help="同精细度（如两个仓库都命中）时数字大的先赢；缺省 0">
          <t-input-number
            v-model="draft.priority"
            :min="0"
            :max="999"
            :step="1"
            theme="normal"
            data-testid="budget-mapping-form-priority"
          />
        </t-form-item>

        <t-form-item label="状态">
          <t-radio-group v-model="draft.status" data-testid="budget-mapping-form-status">
            <t-radio :value="1">启用</t-radio>
            <t-radio :value="0">停用</t-radio>
          </t-radio-group>
        </t-form-item>

        <t-form-item label="备注">
          <t-input v-model="draft.remark" placeholder="可选" data-testid="budget-mapping-form-remark" />
        </t-form-item>
      </t-form>

      <t-alert
        v-if="saveBlocker"
        theme="warning"
        class="form-alert"
        data-testid="budget-mapping-blocker"
      >
        {{ saveBlocker }}
      </t-alert>

      <t-alert v-if="formErrors.length" theme="error" class="form-alert" data-testid="budget-mapping-form-errors">
        <div v-for="(item, index) in formErrors" :key="index">{{ item }}</div>
      </t-alert>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  createBudgetMapping,
  deleteBudgetMapping,
  fetchBudgetAccountOptions,
  fetchBudgetMappingPage,
  updateBudgetMapping,
  type BudgetMapType,
  type BudgetMappingRow,
} from '@/api/business/erp/budget';
import {
  fetchMaterialPage,
  fetchWarehousePage,
  type MaterialRow,
  type Warehouse,
} from '@/api/business/erp';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { MATERIAL_TYPE_OPTIONS } from '@/views/business/erp/material/material-query';
import { dash } from '@/views/business/erp/shared/money';
import {
  BUDGET_PERMS,
  MAP_TYPE_OPTIONS,
  checkMappingDraft,
  draftFromMappingRow,
  emptyMappingDraft,
  emptyMappingFilter,
  mapTypeHint,
  mapTypeLabel,
  mappingFilterParams,
  mappingSaveBlocker,
  mappingStatusLabel,
  mappingStatusTheme,
  mappingTargetText,
  toMappingReq,
  type MappingDraft,
  type MappingFilterState,
} from './budget-logic';

/** 映射的 8 个权限码里，本页只认这一个（增删改都挂在 `budget:mapping` 上） */
const PERM = BUDGET_PERMS.mapping;

const PAGE_SIZE = 20;
const OPTION_LIMIT = 200;

const rows = ref<BudgetMappingRow[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');

const filter = reactive<MappingFilterState>(emptyMappingFilter());

const mapTypeFilterOptions = MAP_TYPE_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const mapTypeOptions = MAP_TYPE_OPTIONS.map((item) => ({ label: `${item.label}（${item.value}）`, value: item.value }));
const materialTypeOptions = MATERIAL_TYPE_OPTIONS.map((item) => ({
  label: `${item.label}（${item.value}）`,
  value: item.value,
}));

const depts = ref<DeptNode[]>([]);
const deptTree = ref<Map<string, string>>(new Map());
const accounts = ref<Array<{ id: string; account_code: string; account_name: string }>>([]);
const materials = ref<MaterialRow[]>([]);
const warehouses = ref<Warehouse[]>([]);
const optionLoading = ref(false);

const deptOptions = computed(() => {
  const out: Array<{ label: string; value: string }> = [];
  const walk = (nodes: DeptNode[], depth: number): void => {
    for (const node of nodes) {
      out.push({ label: `${'　'.repeat(depth)}${node.dept_name}`, value: node.id });
      if (node.children?.length) {
        walk(node.children, depth + 1);
      }
    }
  };
  walk(depts.value, 0);
  return out;
});

const accountOptions = computed(() =>
  accounts.value.map((item) => ({ label: `${item.account_code} ${item.account_name}`, value: item.id })),
);

const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: `${item.material_code} · ${item.material_name}`,
    value: item.id,
  })),
);

const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({
    label: `${item.warehouse_code} · ${item.warehouse_name}`,
    value: item.id,
  })),
);

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

const columns: PrimaryTableCol[] = [
  { colKey: 'map_type', title: '类型', width: 96 },
  { colKey: 'target', title: '映射目标', width: 190, ellipsis: true },
  { colKey: 'account', title: '预算科目', width: 220, ellipsis: true },
  { colKey: 'dept_id', title: '部门覆盖', width: 140, ellipsis: true },
  { colKey: 'priority', title: '优先级', width: 80, align: 'right' },
  { colKey: 'status', title: '状态', width: 82 },
  { colKey: 'remark', title: '备注', ellipsis: true },
  { colKey: 'op', title: '操作', width: 120, fixed: 'right' },
];

// ==================== 表单 ====================

const formVisible = ref(false);
const editingId = ref('');
const submitting = ref(false);
const formErrors = ref<string[]>([]);
const draft = reactive<MappingDraft>(emptyMappingDraft());

/** 后端已知缺陷的拦截面（`MATERIAL_TYPE` 这一版存不进去），文案来自逻辑层 */
const saveBlocker = computed(() => mappingSaveBlocker(draft));

function openCreate(): void {
  editingId.value = '';
  formErrors.value = [];
  Object.assign(draft, emptyMappingDraft());
  formVisible.value = true;
}

function openEdit(row: BudgetMappingRow): void {
  editingId.value = row.id;
  formErrors.value = [];
  Object.assign(draft, draftFromMappingRow(row));
  formVisible.value = true;
}

function closeForm(): void {
  if (submitting.value) {
    return;
  }
  formVisible.value = false;
  editingId.value = '';
  formErrors.value = [];
}

/** 切类型：清掉上一个类型的目标，避免把物料 id 当仓库 id 发出去 */
function handleTypeChange(): void {
  if (draft.map_type === 'DEFAULT') {
    draft.ref_id = '';
    draft.ref_code = '';
  } else if (draft.map_type === 'MATERIAL_TYPE') {
    draft.ref_id = '';
    draft.ref_code = '';
  } else {
    draft.ref_id = '';
    draft.ref_code = '';
  }
  formErrors.value = [];
}

function handleMaterialChange(value: unknown): void {
  const hit = materials.value.find((item) => item.id === value);
  draft.ref_id = hit ? hit.id : '';
  draft.ref_code = hit ? hit.material_code : '';
}

function handleWarehouseChange(value: unknown): void {
  const hit = warehouses.value.find((item) => item.id === value);
  draft.ref_id = hit ? hit.id : '';
  draft.ref_code = hit ? hit.warehouse_code : '';
}

function handleMaterialTypeChange(value: unknown): void {
  // 解析侧只看 ref_code；ref_id 由后端校验强制（= 本版的阻塞点），这里不编造
  draft.ref_code = String(value ?? '');
  draft.ref_id = '';
}

async function submitForm(): Promise<void> {
  if (submitting.value || saveBlocker.value) {
    return;
  }
  const errors = checkMappingDraft(draft);
  formErrors.value = errors;
  if (errors.length) {
    MessagePlugin.warning('请先修正表单里的红字项');
    return;
  }
  submitting.value = true;
  try {
    const payload = toMappingReq(draft);
    if (editingId.value) {
      await updateBudgetMapping(editingId.value, payload);
      MessagePlugin.success('映射已更新');
    } else {
      await createBudgetMapping(payload);
      MessagePlugin.success('映射已创建');
    }
    closeForm();
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    submitting.value = false;
  }
}

function confirmDelete(row: BudgetMappingRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除预算映射',
    body: `确认删除「${mapTypeLabel(row.map_type)} → ${row.account_name}」这条映射？删除后该目标不再占用此预算科目。`,
    theme: 'danger',
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deleteBudgetMapping(row.id);
        MessagePlugin.success('已删除');
        dialog.hide();
        await loadRows();
      } catch (error) {
        MessagePlugin.error(errorText(error));
      }
    },
  });
}

// ==================== 数据加载 ====================

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const page = await fetchBudgetMappingPage(mappingFilterParams(filter, { page: current.value, page_size: PAGE_SIZE }));
    rows.value = page.list ?? [];
    total.value = Number(page.total ?? 0);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `映射列表加载失败：${errorText(error)}`;
  } finally {
    loading.value = false;
  }
}

async function loadDepts(): Promise<void> {
  try {
    depts.value = await getDeptTree();
    const map = new Map<string, string>();
    const walk = (nodes: DeptNode[]): void => {
      for (const node of nodes) {
        map.set(node.id, node.dept_name);
        if (node.children?.length) {
          walk(node.children);
        }
      }
    };
    walk(depts.value);
    deptTree.value = map;
  } catch (error) {
    MessagePlugin.warning(`部门树加载失败：${errorText(error)}`);
  }
}

async function loadAccounts(): Promise<void> {
  try {
    accounts.value = await fetchBudgetAccountOptions(OPTION_LIMIT * 2);
  } catch (error) {
    MessagePlugin.warning(`预算科目加载失败：${errorText(error)}`);
  }
}

async function loadTargets(): Promise<void> {
  optionLoading.value = true;
  try {
    const [materialPage, warehousePage] = await Promise.all([
      fetchMaterialPage({ page: 1, page_size: OPTION_LIMIT, status: 1 }),
      fetchWarehousePage({ page: 1, page_size: OPTION_LIMIT, status: 1 }),
    ]);
    materials.value = materialPage.list ?? [];
    warehouses.value = warehousePage.list ?? [];
  } catch (error) {
    MessagePlugin.warning(`物料 / 仓库下拉加载失败：${errorText(error)}`);
  } finally {
    optionLoading.value = false;
  }
}

function handleSearch(): void {
  current.value = 1;
  void loadRows();
}

function handleReset(): void {
  Object.assign(filter, emptyMappingFilter());
  current.value = 1;
  void loadRows();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadRows();
}

// ==================== 展示小工具 ====================

/** 映射类型的标签主题：越精细越「实在」，兜底给中性色 */
function mapTypeTheme(value: BudgetMapType): 'default' | 'primary' | 'success' | 'warning' {
  switch (value) {
    case 'MATERIAL':
      return 'success';
    case 'WAREHOUSE':
      return 'primary';
    case 'MATERIAL_TYPE':
      return 'warning';
    default:
      return 'default';
  }
}

function deptNameOf(deptId: string | null): string {
  if (!deptId) {
    return '全公司';
  }
  return deptTree.value.get(deptId) ?? deptId.slice(0, 8) + '…';
}

/** 物料类型的可读名留给将来放开 MATERIAL_TYPE 时用（当前用不到，删掉避免未使用告警） */

function errorText(error: unknown): string {
  if (error && typeof error === 'object') {
    const like = error as { message?: string; code?: number };
    const message = (like.message ?? '').trim();
    if (message) {
      return like.code ? `${message}（${like.code}）` : message;
    }
  }
  if (typeof error === 'string' && error.trim()) {
    return error.trim();
  }
  return '请求失败，请稍后重试';
}

onMounted(() => {
  void loadDepts();
  void loadAccounts();
  void loadTargets();
  void loadRows();
});
</script>

<style scoped lang="less">
.mapping-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;

  &__spacer {
    flex: 1;
  }

  &__type {
    width: 150px;
  }

  &__dept {
    width: 180px;
  }
}

.list-alert {
  margin-bottom: 12px;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;

  &__main {
    display: flex;
    align-items: baseline;
    gap: 8px;
    white-space: nowrap;
  }

  &__title {
    font-size: 16px;
    font-weight: 600;
  }

  &__meta {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }

  &__tip {
    color: var(--td-text-color-secondary);
    font-size: 12px;
    text-align: right;
  }
}

.name-cell {
  display: flex;
  flex-direction: column;
  line-height: 1.35;

  &__main {
    font-weight: 500;
  }

  &__sub {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

.op-cell {
  display: flex;
  gap: 8px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
  font-size: 13px;
}

.muted {
  color: var(--td-text-color-placeholder);
}

.form-alert {
  margin-top: 8px;
}
</style>
