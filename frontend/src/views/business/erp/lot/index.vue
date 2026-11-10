<template>
  <div class="lot-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-lot-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建批次
        </t-button>
        <t-button variant="outline" data-testid="btn-lot-refresh" @click="refresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.material_id"
          class="tool-row__material"
          :options="materialOptions"
          placeholder="全部物料"
          clearable
          filterable
          data-testid="lot-filter-material"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.source_type"
          class="tool-row__source"
          :options="sourceFilterOptions"
          placeholder="全部来源"
          clearable
          data-testid="lot-filter-source"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.qc_status"
          class="tool-row__qc"
          :options="qcFilterOptions"
          placeholder="全部质检"
          clearable
          data-testid="lot-filter-qc"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="lot-filter-status"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="批号 / 物料编码 / 名称（模糊）"
          clearable
          data-testid="lot-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-lot-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-lot-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">批次管理</span>
          <span class="panel-head__meta" data-testid="lot-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">
          批号在同一物料下唯一（不同物料可以重名）；停用的批次仍保留历史，但不再出现在领料下拉里
        </span>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert" data-testid="lot-load-error">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadRows()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="lot-table"
        @page-change="handlePageChange"
      >
        <template #lot_no="{ row }">
          <span class="mono" :data-testid="`lot-no-${row.lot_no}`">{{ row.lot_no }}</span>
        </template>

        <template #material_id="{ row }">
          <span class="mono">{{ row.material_code || shortId(row.material_id) }}</span>
          <span class="muted"> {{ row.material_name || '' }}</span>
        </template>

        <template #source_type="{ row }">
          <t-tag size="small" variant="outline" :theme="sourceTheme(row.source_type)">
            {{ sourceTypeLabel(row.source_type) }}
          </t-tag>
        </template>

        <template #prod_date="{ row }">
          <span class="muted">{{ dash(row.prod_date) }}</span>
        </template>

        <template #expire_date="{ row }">
          <span class="muted">{{ dash(row.expire_date) }}</span>
        </template>

        <template #qc_status="{ row }">
          <t-tag size="small" :theme="qcStatusTheme(row.qc_status)">
            {{ qcStatusLabel(row.qc_status) }}
          </t-tag>
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="lotStatusTheme(row.status)" data-testid="lot-row-status">
            {{ lotStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <t-link v-privilege="[PERM.update]" theme="primary" :data-testid="`btn-lot-edit-${row.lot_no}`" @click="openEdit(row)">
            编辑
          </t-link>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="lot-empty">
            <t-icon name="inbox" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的批次' : '还没有批次' }}</p>
            <p class="panel-empty__desc">
              {{ filtered ? '换个条件再查，或点「重置」回全量' : '采购到货 / 生产产出会自动建批，也可以点「新建批次」手工建档' }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑抽屉 ==================== -->
    <t-drawer
      v-model:visible="formVisible"
      :header="form.id ? '编辑批次' : '新建批次'"
      size="620px"
      :footer="false"
      data-testid="lot-form-drawer"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" data-testid="lot-submit-error" />

      <t-form :data="form" label-width="96px" class="form">
        <t-form-item label="批号" name="lot_no" :status="formErrors.lot_no ? 'error' : undefined" :tips="formErrors.lot_no">
          <t-input
            v-model="form.lot_no"
            :maxlength="LOT_NO_MAX"
            placeholder="同一物料下不能重复（≤64 字）"
            data-testid="lot-form-no"
          />
        </t-form-item>

        <t-form-item
          label="物料"
          name="material_id"
          :status="formErrors.material_id ? 'error' : undefined"
          :tips="formErrors.material_id"
        >
          <t-select
            v-model="form.material_id"
            :options="materialOptions"
            :disabled="Boolean(form.id)"
            placeholder="选择物料"
            filterable
            data-testid="lot-form-material"
          />
          <div v-if="form.id" class="field-hint">物料是业务键的一半，(物料, 批号) 唯一 —— 建单后不可改</div>
        </t-form-item>

        <div class="form-row">
          <t-form-item label="来源类型" name="source_type" class="form-row__col">
            <t-select v-model="form.source_type" :options="sourceFilterOptions" data-testid="lot-form-source" />
          </t-form-item>
          <t-form-item label="质检状态" name="qc_status" class="form-row__col">
            <t-select v-model="form.qc_status" :options="qcFilterOptions" data-testid="lot-form-qc" />
          </t-form-item>
        </div>

        <div class="form-row">
          <t-form-item label="生产日期" name="prod_date" class="form-row__col">
            <t-date-picker
              v-model="form.prod_date"
              format="YYYY-MM-DD"
              clearable
              placeholder="可选"
              data-testid="lot-form-prod-date"
            />
          </t-form-item>
          <t-form-item
            label="失效日期"
            name="expire_date"
            class="form-row__col"
            :status="formErrors.expire_date ? 'error' : undefined"
            :tips="formErrors.expire_date"
          >
            <t-date-picker
              v-model="form.expire_date"
              format="YYYY-MM-DD"
              clearable
              placeholder="不得早于生产日期"
              data-testid="lot-form-expire-date"
            />
          </t-form-item>
        </div>

        <t-form-item label="状态" name="status">
          <t-radio-group :value="form.status" data-testid="lot-form-status" @change="handleStatusChange">
            <t-radio :value="LOT_STATUS_ENABLED">启用</t-radio>
            <t-radio :value="LOT_STATUS_DISABLED">停用</t-radio>
          </t-radio-group>
          <div class="field-hint">停用后不再出现在领料 / 成本对象的批号下拉里，历史单据不受影响</div>
        </t-form-item>

        <t-form-item label="备注" name="remark">
          <t-textarea
            v-model="form.remark"
            :maxlength="1000"
            :autosize="{ minRows: 2, maxRows: 4 }"
            placeholder="选填"
            data-testid="lot-form-remark"
          />
        </t-form-item>
      </t-form>

      <div class="drawer-footer">
        <t-button theme="primary" :loading="submitting" :disabled="submitting" data-testid="btn-lot-save" @click="handleSave">
          {{ form.id ? '保存修改' : '保存' }}
        </t-button>
        <t-button variant="outline" data-testid="btn-lot-form-cancel" @click="formVisible = false">取消</t-button>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 批次管理页（COST-B1-12 / ARCH-9 §5.2 #7–#9 · §6.1 #6）。
 *
 * # 这一页只用三个接口（后端 B1-9 已交付）
 *
 * | 动作 | 接口 | 权限码 |
 * |---|---|---|
 * | 列表（**可按物料筛**） | `GET /business/erp/lot` | `business:erp:lot:list` |
 * | 新建 | `POST /business/erp/lot` | `business:erp:lot:create` |
 * | 编辑（部分更新） | `PUT /business/erp/lot/{id}` | `business:erp:lot:update` |
 *
 * **没有删除按钮**：设计 §5.3 只给 3 个码、后端没有 DELETE 路由 —— 挂一个必 404 的按钮
 * 比没有更糟。批次作废属于下游卡（增量 / 作废）。
 *
 * # 三个实现口径（都写在 `lot-logic.ts` 并单测钉住）
 *
 * 1. 编辑只发**真改过**的字段（后端三态：缺失 = 不改、`null` = 清空）；
 * 2. `material_id` 编辑态只读（业务键不可变，后端入参里也没有这个字段）；
 * 3. 批号撞唯一键 → 后端 409 + `66056`，报文原样上屏（带物料名与批号）。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import type { PrimaryTableCol } from 'tdesign-vue-next';

import { fetchMaterialPage, type MaterialRow } from '@/api/business/erp';
import {
  LOT_NO_MAX,
  LOT_PAGE_SIZE,
  LOT_PERMISSIONS,
  LOT_STATUS_DISABLED,
  LOT_STATUS_ENABLED,
  createLot,
  fetchLotPage,
  updateLot,
  QC_STATUS_OPTIONS,
  SOURCE_TYPE_OPTIONS,
  type LotPageQuery,
  type LotRow,
} from '@/api/business/erp/lot';
import {
  buildLotQuery,
  dash,
  emptyLotFilter,
  emptyLotForm,
  errorText,
  formFromRow,
  isEmptyUpdate,
  isLotFiltered,
  lotStatusLabel,
  lotStatusTheme,
  qcStatusLabel,
  qcStatusTheme,
  shortId,
  sourceTypeLabel,
  toCreatePayload,
  toUpdatePayload,
  validateLotForm,
  type LotFilterState,
  type LotFormErrors,
  type LotFormState,
} from './lot-logic';

/** 本页权限码（与 V0378 的菜单 / 按钮种子一一对应） */
const PERM = LOT_PERMISSIONS;

/** 主数据下拉一次拉多少（与收货 / 出库页同口径） */
const OPTION_LIMIT = 200;

const columns: PrimaryTableCol[] = [
  { colKey: 'lot_no', title: '批号', width: 180 },
  { colKey: 'material_id', title: '物料', minWidth: 220, ellipsis: true },
  { colKey: 'source_type', title: '来源', width: 100 },
  { colKey: 'prod_date', title: '生产日期', width: 110 },
  { colKey: 'expire_date', title: '失效日期', width: 110 },
  { colKey: 'qc_status', title: '质检', width: 100 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'op', title: '操作', width: 90, fixed: 'right' },
];

const sourceFilterOptions = SOURCE_TYPE_OPTIONS.map((item) => ({ ...item }));
const qcFilterOptions = QC_STATUS_OPTIONS.map((item) => ({ ...item }));
const statusFilterOptions = [
  { label: '启用', value: String(LOT_STATUS_ENABLED) },
  { label: '停用', value: String(LOT_STATUS_DISABLED) },
];

// ==================== 列表 ====================
const filter = reactive<LotFilterState>(emptyLotFilter());
const rows = ref<LotRow[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = LOT_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

const filtered = computed(() => isLotFiltered(filter));

const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// ==================== 物料主数据 ====================
const materials = ref<MaterialRow[]>([]);
const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: `${item.material_code} ${item.material_name}`,
    value: item.id,
  })),
);

async function loadMaterials(): Promise<void> {
  try {
    const page = await fetchMaterialPage({ page: 1, page_size: OPTION_LIMIT });
    materials.value = page.list;
  } catch {
    // 主数据拉不到不挡列表：下拉会空，列表照常
  }
}

async function loadRows(page = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const query: LotPageQuery = buildLotQuery(filter, page, pageSize);
    const data = await fetchLotPage(query);
    rows.value = data.list;
    total.value = data.total;
    current.value = page;
  } catch (error) {
    loadError.value = errorText(error);
    rows.value = [];
    total.value = 0;
  } finally {
    loading.value = false;
  }
}

function refresh(): void {
  void loadRows(current.value);
}

function handleSearch(): void {
  void loadRows(1);
}

function handleReset(): void {
  Object.assign(filter, emptyLotFilter());
  void loadRows(1);
}

function handlePageChange(pageInfo: { current: number }): void {
  void loadRows(pageInfo.current);
}

/** 来源标签主题 */
function sourceTheme(value: string): 'default' | 'primary' | 'warning' {
  switch (value) {
    case 'PRODUCTION':
      return 'primary';
    case 'PURCHASE':
      return 'warning';
    default:
      return 'default';
  }
}

// ==================== 表单 ====================
const formVisible = ref(false);
const form = reactive<LotFormState>(emptyLotForm());
/** 编辑原值（算差异用） */
let formOriginal: LotFormState = emptyLotForm();
const formErrors = ref<LotFormErrors>({});
const submitting = ref(false);
const submitError = ref('');

function resetForm(next: LotFormState): void {
  Object.assign(form, next);
  formOriginal = { ...next };
  formErrors.value = {};
  submitError.value = '';
}

function openCreate(): void {
  resetForm(emptyLotForm());
  formVisible.value = true;
}

function openEdit(row: LotRow): void {
  resetForm(formFromRow(row));
  formVisible.value = true;
}

function handleStatusChange(value: string | number | boolean): void {
  form.status = Number(value) === LOT_STATUS_DISABLED ? LOT_STATUS_DISABLED : LOT_STATUS_ENABLED;
}

async function handleSave(): Promise<void> {
  submitError.value = '';
  formErrors.value = validateLotForm(form);
  if (Object.keys(formErrors.value).length > 0) {
    return;
  }

  submitting.value = true;
  try {
    if (form.id) {
      const payload = toUpdatePayload(formOriginal, form);
      if (isEmptyUpdate(payload)) {
        MessagePlugin.info('没有改动，无需保存');
        formVisible.value = false;
        return;
      }
      await updateLot(form.id, payload);
      MessagePlugin.success('批次已更新');
    } else {
      await createLot(toCreatePayload(form));
      MessagePlugin.success('批次已创建');
    }
    formVisible.value = false;
    await loadRows(form.id ? current.value : 1);
  } catch (error) {
    // 批号重复（409 / 66056）的报文带物料名与批号 —— 原样上屏，别只写「保存失败」
    submitError.value = errorText(error);
  } finally {
    submitting.value = false;
  }
}

onMounted(() => {
  void loadMaterials();
  void loadRows(1);
});
</script>

<style scoped>
.lot-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 12px;
}

.tool-card,
.list-card {
  border-radius: 8px;
}

.tool-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.tool-row__spacer {
  flex: 1;
}

.tool-row__material {
  width: 220px;
}

.tool-row__source,
.tool-row__qc,
.tool-row__status {
  width: 130px;
}

.tool-row__keyword {
  width: 240px;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  margin-bottom: 8px;
}

.panel-head__title {
  font-size: 16px;
  font-weight: 600;
}

.panel-head__meta {
  margin-left: 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.panel-head__tip {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.list-alert,
.dlg-alert {
  margin-bottom: 12px;
}

.panel-empty {
  padding: 32px 0;
  text-align: center;
}

.panel-empty__icon {
  font-size: 40px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 8px 0 4px;
  font-weight: 600;
}

.panel-empty__desc {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.form-row {
  display: flex;
  gap: 12px;
}

.form-row__col {
  flex: 1;
}

.field-hint {
  margin-top: 4px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 1.4;
}

.drawer-footer {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  margin-top: 16px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.muted {
  color: var(--td-text-color-secondary);
}
</style>
