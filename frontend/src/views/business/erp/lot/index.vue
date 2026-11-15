<template>
  <div class="lot-page">
    <!-- ==================== ①② 筛选 / 分页 / 行操作 交给 ListPage ==================== -->
    <ListPage
      ref="listRef"
      title="批次管理"
      testid="lot"
      table-testid="lot-table"
      meta-testid="lot-meta"
      btn-prefix="btn-lot"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="LOT_PAGE_SIZE"
      exportable
      @loaded="onLoaded"
    >
      <template #head-extra>
        <span class="panel-head__tip">
          批号在同一物料下唯一（不同物料可以重名）；停用的批次仍保留历史，但不再出现在领料下拉里
        </span>
      </template>

      <template #toolbar>
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="btn-lot-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建批次
        </t-button>
      </template>

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
        <div class="panel-empty panel-empty--doc" data-testid="lot-empty">
          <t-icon name="inbox" class="panel-empty__icon" />
          <p class="panel-empty__title">{{ filtered ? '没有匹配的批次' : '还没有批次' }}</p>
          <p class="panel-empty__desc">
            {{ filtered ? '换个条件再查，或点「重置」回全量' : '采购到货 / 生产产出会自动建批，也可以点「新建批次」手工建档' }}
          </p>
        </div>
      </template>
    </ListPage>

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
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
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

// ==================== 列表（筛选 / 分页 / 导出交给 ListPage） ====================

/** 筛选行（`filterState` 就地读写；`buildLotQuery` 等闭包引用的是同一个对象） */
const filter = reactive<LotFilterState>(emptyLotFilter());
/** 当前页码（保存后回页用；每拉完一页由 `onLoaded` 同步） */
const current = ref(1);
/** ListPage 实例（新建 / 编辑后重拉列表） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 是否处于筛选态（空态文案分「没数据」「没匹配」） */
const filtered = computed(() => isLotFiltered(filter));

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

/**
 * 筛选项声明。
 *
 * 五个控件改造前**都**是「值一变就查」（`@change="handleSearch"`），
 * 所以下拉一律 `searchOnChange`，关键词走回车 —— 手感与改造前一致。
 */
const filters = computed<FilterDef[]>(() => [
  {
    key: 'material_id',
    type: 'select',
    placeholder: '全部物料',
    width: 220,
    options: materialOptions.value,
    filterable: true,
    searchOnChange: true,
    testid: 'lot-filter-material',
  },
  {
    key: 'source_type',
    type: 'select',
    placeholder: '全部来源',
    width: 130,
    options: sourceFilterOptions,
    searchOnChange: true,
    testid: 'lot-filter-source',
  },
  {
    key: 'qc_status',
    type: 'select',
    placeholder: '全部质检',
    width: 130,
    options: qcFilterOptions,
    searchOnChange: true,
    testid: 'lot-filter-qc',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 130,
    options: statusFilterOptions,
    searchOnChange: true,
    testid: 'lot-filter-status',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '批号 / 物料编码 / 名称（模糊）',
    width: 240,
    testid: 'lot-filter-keyword',
  },
]);

/** 拉一页（入参拼装沿用 `buildLotQuery`） */
const fetcher: ListFetcher<LotRow> = (query) => fetchLotPage(query as LotPageQuery);

/** 入参拼装（页面自己的 `buildLotQuery`，不换接口契约） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildLotQuery(state as LotFilterState, page, pageSize);

/** 重拉到指定页（不传 = 当前页） */
async function reloadList(page: number = current.value): Promise<void> {
  await listRef.value?.load(page);
}

/** 一页拉完：同步页码（保存后回页要用） */
function onLoaded(payload: { page: number }): void {
  current.value = payload.page;
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
    await reloadList(form.id ? current.value : 1);
  } catch (error) {
    // 批号重复（409 / 66056）的报文带物料名与批号 —— 原样上屏，别只写「保存失败」
    submitError.value = errorText(error);
  } finally {
    submitting.value = false;
  }
}

onMounted(() => {
  void loadMaterials();
});
</script>

<style scoped>
.lot-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 12px;
}

.list-alert,
.dlg-alert {
  margin-bottom: 12px;
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
