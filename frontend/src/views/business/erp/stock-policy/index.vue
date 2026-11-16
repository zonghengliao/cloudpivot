<template>
  <div class="stock-policy-page">
    <!-- 主数据下拉缺读权限时给一句人话：列表本身不受影响，别让人以为「页面坏了」 -->
    <t-alert
      v-if="masterGaps.length"
      theme="info"
      class="sp-alert"
      data-testid="stock-policy-master-gap"
      :message="`以下下拉不可用（缺主数据读权限，列表与写接口不受影响）：${masterGaps.join(' / ')}`"
    />

    <!-- 没有列表码：进得来（有页面显形码）但列表必然 403 —— 说清缺什么 -->
    <t-alert
      v-if="!canList"
      theme="warning"
      class="sp-alert"
      data-testid="stock-policy-no-perm"
      :message="`当前账号没有 ${PERM_STOCK_POLICY_LIST} 权限，列表不可用；请让管理员在角色授权里勾上「业务 → 仓库 → 即时库存 → 水位设置列表」`"
    />

    <t-alert
      theme="info"
      class="sp-alert"
      data-testid="stock-policy-hint"
      message="一格 = 一个「物料 × 仓库」的最低 / 最高水位。最低水位 = 该格的安全库存阈值（账面结存低于它 → 水位检查生成待办）；最高水位留空 = 不判超储。0 表示这一格明示不预警（会阻断物料级安全库存的回退）。"
    />

    <ListPage
      ref="listRef"
      title="安全库存水位"
      testid="stock-policy"
      table-testid="stock-policy-table"
      meta-testid="stock-policy-meta"
      meta-unit="格"
      btn-prefix="stock-policy"
      row-key="id"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="STOCK_POLICY_PAGE_SIZE"
      :actions="actions"
      @loaded="onLoaded"
    >
      <template #meta-extra>
        <template v-if="pageRows.length">
          · 本页 {{ enabledOnPage }} 格启用 / {{ pageRows.length - enabledOnPage }} 格停用
        </template>
      </template>

      <template #toolbar>
        <ImportEntryButton
          :biz-type="STOCK_POLICY_IMPORT_BIZ_TYPE"
          biz-label="安全库存水位"
          testid="btn-import-stock-policy"
        />

        <t-tooltip
          :content="canSave ? '一次给多个物料 × 多个仓库设置同一个水位' : noSaveTip"
          placement="top-right"
        >
          <span class="sp-gate">
            <t-button variant="outline" data-testid="btn-stock-policy-batch" :disabled="!canSave" @click="openBatch">
              <template #icon><t-icon name="layers" /></template>
              批量设置
            </t-button>
          </span>
        </t-tooltip>

        <t-tooltip :content="canSave ? '设置一格的最高 / 最低水位' : noSaveTip" placement="top-right">
          <span class="sp-gate">
            <t-button theme="primary" data-testid="btn-stock-policy-new" :disabled="!canSave" @click="openCreate">
              <template #icon><t-icon name="add" /></template>
              新建水位
            </t-button>
          </span>
        </t-tooltip>
      </template>

      <template #material="{ row }">
        <div class="sp-cell">
          <span class="sp-cell__main" :data-testid="`stock-policy-material-${row.id}`">{{ materialText(row) }}</span>
          <span class="sp-cell__sub">{{ row.spec || '—' }}<template v-if="row.unit_name"> · {{ row.unit_name }}</template></span>
        </div>
      </template>

      <template #warehouse="{ row }">{{ warehouseText(row) }}</template>

      <template #min_qty="{ row }">
        <span class="sp-num">{{ minQtyText(row.min_qty) }}</span>
      </template>

      <template #max_qty="{ row }">
        <span class="sp-num" :class="{ 'sp-muted': row.max_qty === null }">{{ formatQty(row.max_qty) }}</span>
      </template>

      <template #status="{ row }">
        <t-tag size="small" :theme="statusTheme(row.status)" variant="light" data-testid="stock-policy-status-tag">
          {{ statusLabel(row.status) }}
        </t-tag>
      </template>

      <template #remark="{ row }">
        <span class="sp-muted">{{ row.remark || '—' }}</span>
      </template>

      <template #updated_at="{ row }">
        <span class="sp-muted sp-mono">{{ dateTimeText(row.updated_at) }}</span>
      </template>

      <template #empty="{ filtered }">
        <div class="sp-empty" data-testid="stock-policy-empty">{{ emptyText(Boolean(filtered)) }}</div>
      </template>
    </ListPage>

    <!-- ==================== 单格新建 / 编辑 ==================== -->
    <t-dialog
      v-model:visible="cellVisible"
      :header="isEdit ? '编辑水位（物料 × 仓库 不可改）' : '新建水位'"
      :confirm-btn="{ content: isEdit ? '保存' : '创建', loading: saving }"
      :close-on-overlay-click="false"
      width="560px"
      data-testid="stock-policy-cell-dialog"
      @confirm="submitCell"
    >
      <t-alert v-if="submitError" theme="error" class="sp-alert" :message="submitError" data-testid="stock-policy-cell-error" />

      <t-form label-align="right" :label-width="96">
        <t-form-item label="物料" required-mark>
          <t-select
            v-model="cellForm.material_id"
            :options="materialOptions"
            :disabled="isEdit"
            filterable
            clearable
            placeholder="选择物料"
            data-testid="stock-policy-cell-material"
          />
          <div v-if="isEdit" class="sp-hint">
            物料 / 仓库是这一格的键：要换格请删掉这一行再新建（换格 = 删一格 + 设另一格）
          </div>
        </t-form-item>

        <t-form-item label="仓库" required-mark>
          <t-select
            v-model="cellForm.warehouse_id"
            :options="warehouseOptions"
            :disabled="isEdit"
            filterable
            clearable
            placeholder="选择仓库"
            data-testid="stock-policy-cell-warehouse"
          />
        </t-form-item>

        <t-form-item label="最低水位" required-mark :help="levelError?.field === 'min_qty' ? levelError.message : '账面结存 < 它 → 判低于水位；填 0 = 这一格不预警'">
          <t-input
            v-model="cellForm.min_qty"
            placeholder="如 100"
            data-testid="stock-policy-cell-min"
          >
            <template #suffix>数量</template>
          </t-input>
        </t-form-item>

        <t-form-item label="最高水位" :help="levelError?.field === 'max_qty' ? levelError.message : '留空 = 不判超储；填了必须严格大于最低水位'">
          <t-input
            v-model="cellForm.max_qty"
            placeholder="留空 = 不判超储"
            data-testid="stock-policy-cell-max"
          >
            <template #suffix>数量</template>
          </t-input>
        </t-form-item>

        <t-form-item label="状态">
          <t-radio-group v-model="cellForm.status" data-testid="stock-policy-cell-status">
            <t-radio :value="1">启用</t-radio>
            <t-radio :value="0">停用</t-radio>
          </t-radio-group>
          <div class="sp-hint">{{ statusHint(cellForm.status) }}</div>
        </t-form-item>

        <t-form-item label="备注">
          <t-input v-model="cellForm.remark" placeholder="为什么这么设（≤200 字，可留空）" data-testid="stock-policy-cell-remark" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ==================== 批量设置 ==================== -->
    <t-dialog
      v-model:visible="batchVisible"
      header="批量设置水位（物料 × 仓库）"
      :confirm-btn="{ content: `设置 ${batchPreview.cells || 0} 格`, loading: batchSaving, disabled: Boolean(batchError) }"
      :close-on-overlay-click="false"
      width="640px"
      data-testid="stock-policy-batch-dialog"
      @confirm="submitBatch"
    >
      <t-alert v-if="batchError" theme="error" class="sp-alert" :message="batchError" data-testid="stock-policy-batch-error" />
      <t-alert
        v-else
        :theme="batchPreview.overLimit ? 'warning' : 'info'"
        class="sp-alert"
        :message="batchPreviewText(batchPreview)"
        data-testid="stock-policy-batch-preview"
      />

      <t-form label-align="right" :label-width="96">
        <t-form-item label="物料" required-mark>
          <t-select
            v-model="batchForm.material_ids"
            :options="materialOptions"
            multiple
            filterable
            clearable
            placeholder="可多选（至少 1 个）"
            data-testid="stock-policy-batch-materials"
          />
        </t-form-item>

        <t-form-item label="仓库" required-mark>
          <t-select
            v-model="batchForm.warehouse_ids"
            :options="warehouseOptions"
            multiple
            filterable
            clearable
            placeholder="可多选（至少 1 个）"
            data-testid="stock-policy-batch-warehouses"
          />
        </t-form-item>

        <t-form-item label="最低水位" required-mark>
          <t-input v-model="batchForm.min_qty" placeholder="如 100（这些格都设成它）" data-testid="stock-policy-batch-min" />
        </t-form-item>

        <t-form-item label="最高水位">
          <t-input v-model="batchForm.max_qty" placeholder="留空 = 不判超储" data-testid="stock-policy-batch-max" />
        </t-form-item>

        <t-form-item label="状态">
          <t-radio-group v-model="batchForm.status" data-testid="stock-policy-batch-status">
            <t-radio :value="1">启用</t-radio>
            <t-radio :value="0">停用</t-radio>
          </t-radio-group>
        </t-form-item>

        <t-form-item label="备注">
          <t-input v-model="batchForm.remark" placeholder="整批一份备注（可留空）" data-testid="stock-policy-batch-remark" />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 安全库存设置页（M2-17）—— 单格设置 + **批量设置** + **Excel 导入**。
 *
 * # 一张表，三个入口
 *
 * | 入口 | 后端 | 权限码 | 一次影响 |
 * |------|------|--------|----------|
 * | 新建 / 编辑（弹窗） | `PUT …/stock-policy`（M2-13，upsert） | `…:stock-policy:save` | 1 格 |
 * | 批量设置（弹窗） | `PUT …/stock-policy/batch`（M2-17） | `…:stock-policy:save` | 物料 × 仓库（≤2 000 格，一个事务） |
 * | 导入（`ImportEntryButton`） | 通用导入向导 `biz_type=business.erp.stock-policy`（M2-17） | impexp 模板 / 上传码 + `…:stock-policy:save` 语义 | 一份 Excel（≤20 000 行，一个事务） |
 * | 删除（行内） | `DELETE …/stock-policy/{id}`（M2-13，软删） | `…:stock-policy:delete` | 1 格（回退物料级） |
 *
 * 三个入口写的是**同一张表、同一份列、同一套约束**（`ck_biz_msp_min` / `ck_biz_msp_max` /
 * `ck_biz_msp_status` + `uk_biz_msp_mat_wh`），差别只在「一次来几格」和「从哪来」。
 * 页面不重算水位、不重算结存：结存永远由后端现算（M2-14 水位检查 / M2-15 补货建议）。
 *
 * # 页面显形码与接口码分开
 *
 * 侧栏 / 路由用 `business:erp:stock-policy:page`（V0431 页面行，后端零引用）；
 * 四个接口各自判自己的码。所以本页按码决定按钮是否可点，而不是让用户点了才吃 403：
 * 没有 `list` 码 → 顶部黄条说明列表不可用；没有 `save` 码 → 新建 / 批量置灰 + tooltip。
 *
 * # 为什么「物料 / 仓库」在编辑时不能改
 *
 * 它们是这一格的**键**（`WHERE` 条件，见后端 `model::policy_update`）：换格 = 删一格 + 设另一格，
 * 混成一个动作会让旧格悄悄回退物料级而没人注意。所以要换格就删了重建。
 *
 * # 导入为什么走通用向导
 *
 * Excel 解析、表头校验、逐格校验、错误行 xlsx、确认入库这套能力 impexp 已经全部有
 * （物料 / 仓库 / 员工档案都在用）。在页面里再写一份 xlsx 解析就是第二套口径 ——
 * 而「两套口径」在这类批量写数据的场景里迟早会出现「页面能导的、向导导不了」。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';

import ListPage from '@/components/ListPage/index.vue';
import ImportEntryButton from '@/components/ImportEntryButton.vue';
import type { ColumnDef, FilterDef, ListActions, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import {
  fetchMaterialPage,
  fetchWarehousePage,
  type MaterialRow,
  type Warehouse,
} from '@/api/business/erp';
import {
  deleteStockPolicy,
  fetchStockPolicyPage,
  saveStockPolicy,
  saveStockPolicyBatch,
  type StockPolicyRow,
} from '@/api/business/erp/stock-policy';
import { useUserStore } from '@/stores/user';

import {
  PERM_STOCK_POLICY_DELETE,
  PERM_STOCK_POLICY_LIST,
  PERM_STOCK_POLICY_SAVE,
  STOCK_POLICY_IMPORT_BIZ_TYPE,
  STOCK_POLICY_PAGE_SIZE,
  STOCK_POLICY_STATUS_FILTER_OPTIONS,
  batchPreviewText,
  buildStockPolicyBatchPayload,
  buildStockPolicyQuery,
  buildStockPolicySavePayload,
  emptyStockPolicyBatchForm,
  emptyStockPolicyCellForm,
  emptyStockPolicyFilter,
  emptyText,
  formFromStockPolicyRow,
  formatQty,
  materialText,
  minQtyText,
  previewStockPolicyBatch,
  requestErrorMessage,
  statusHint,
  statusLabel,
  statusTheme,
  validateStockPolicyBatch,
  validateStockPolicyLevels,
  warehouseText,
  type StockPolicyBatchForm,
  type StockPolicyCellForm,
  type StockPolicyFilterState,
} from './stock-policy-logic';

// ==================== 权限 ====================

const userStore = useUserStore();
const canList = computed(() => userStore.hasPermission(PERM_STOCK_POLICY_LIST));
const canSave = computed(() => userStore.hasPermission(PERM_STOCK_POLICY_SAVE));
const canDelete = computed(() => userStore.hasPermission(PERM_STOCK_POLICY_DELETE));
const noSaveTip = `当前账号没有 ${PERM_STOCK_POLICY_SAVE} 权限`;

// ==================== 主数据下拉（能拉就拉） ====================

const materials = ref<MaterialRow[]>([]);
const warehouses = ref<Warehouse[]>([]);
/** 拉不到的主数据（缺权限 / 接口错）：给一句人话，不让下拉静默变空 */
const masterGaps = ref<string[]>([]);

const materialOptions = computed(() =>
  materials.value.map((item) => ({ label: `${item.material_code} ${item.material_name}`, value: item.id })),
);
const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({ label: `${item.warehouse_code} ${item.warehouse_name}`, value: item.id })),
);

/** 主数据下拉：缺读权限就不发请求（发了必然 403，还会让全局拦截器弹一次错） */
async function loadMasterData(): Promise<void> {
  const gaps: string[] = [];
  const tasks: Array<Promise<void>> = [];
  if (userStore.hasPermission('business:erp:material:list')) {
    tasks.push(
      fetchMaterialPage({ page: 1, page_size: 200 })
        .then((res) => {
          materials.value = res.list;
        })
        .catch(() => {
          gaps.push('物料');
        }),
    );
  } else {
    gaps.push('物料');
  }
  if (userStore.hasPermission('business:erp:warehouse:list')) {
    tasks.push(
      fetchWarehousePage({ page: 1, page_size: 200 })
        .then((res) => {
          warehouses.value = res.list;
        })
        .catch(() => {
          gaps.push('仓库');
        }),
    );
  } else {
    gaps.push('仓库');
  }
  await Promise.all(tasks);
  masterGaps.value = gaps;
}

// ==================== 列表 ====================

const listRef = ref<InstanceType<typeof ListPage> | null>(null);
const filter = reactive<StockPolicyFilterState>(emptyStockPolicyFilter());
const pageRows = ref<StockPolicyRow[]>([]);
const current = ref(1);

/** 本页启用的格数（页头副题：「本页 N 格启用 / M 格停用」——停用的格不参与水位检查） */
const enabledOnPage = computed(() => pageRows.value.filter((row) => row.status !== 0).length);

const columns: ColumnDef[] = [
  { colKey: 'material', title: '物料', minWidth: 240 },
  { colKey: 'warehouse', title: '仓库', width: 180 },
  { colKey: 'min_qty', title: '最低水位', width: 130, align: 'right' },
  { colKey: 'max_qty', title: '最高水位', width: 130, align: 'right' },
  { colKey: 'status', title: '状态', width: 100, align: 'center' },
  { colKey: 'remark', title: '备注', minWidth: 160, ellipsis: true },
  { colKey: 'updated_at', title: '更新时间', width: 170 },
];

/** 筛选项：物料 / 仓库 / 状态都走下拉（几百条物料必须 filterable）；`computed` 让主数据拉回来后下拉自动重建 */
const filters = computed<FilterDef[]>(() => [
  { key: 'keyword', type: 'keyword', placeholder: '搜索物料 / 仓库编码或名称', width: 260, testid: 'stock-policy-keyword' },
  {
    key: 'material_id',
    type: 'select',
    placeholder: '全部物料',
    width: 220,
    options: materialOptions.value,
    filterable: true,
    testid: 'stock-policy-filter-material',
  },
  {
    key: 'warehouse_id',
    type: 'select',
    placeholder: '全部仓库',
    width: 200,
    options: warehouseOptions.value,
    filterable: true,
    testid: 'stock-policy-filter-warehouse',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 130,
    options: STOCK_POLICY_STATUS_FILTER_OPTIONS.map((item) => ({ ...item })),
    testid: 'stock-policy-filter-status',
  },
]);

const fetcher: ListFetcher<StockPolicyRow> = (query) => fetchStockPolicyPage(query as Parameters<typeof fetchStockPolicyPage>[0]);
const queryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildStockPolicyQuery(state as StockPolicyFilterState, page, pageSize);

/** 一页拉完：镜像行 + 同步页码（页头副题与删除后回页用） */
function onLoaded(payload: { rows: StockPolicyRow[]; page: number }): void {
  pageRows.value = payload.rows;
  current.value = payload.page;
}

/** 重拉到指定页（不传 = 当前页） */
async function reloadList(page: number = current.value): Promise<void> {
  await listRef.value?.load(page);
}

// ==================== 单格新建 / 编辑 ====================

const cellVisible = ref(false);
const editing = ref<StockPolicyRow | null>(null);
const cellForm = reactive<StockPolicyCellForm>(emptyStockPolicyCellForm());
const saving = ref(false);
const submitError = ref('');
const isEdit = computed(() => editing.value !== null);
const levelError = computed(() => validateStockPolicyLevels(cellForm.min_qty, cellForm.max_qty, cellForm.status));

function openCreate(): void {
  if (!canSave.value) {
    return;
  }
  editing.value = null;
  Object.assign(cellForm, emptyStockPolicyCellForm());
  submitError.value = '';
  cellVisible.value = true;
}

function openEdit(row: StockPolicyRow): void {
  editing.value = row;
  Object.assign(cellForm, formFromStockPolicyRow(row));
  submitError.value = '';
  cellVisible.value = true;
}

async function submitCell(): Promise<void> {
  submitError.value = '';
  if (!cellForm.material_id) {
    submitError.value = '请选择物料';
    return;
  }
  if (!cellForm.warehouse_id) {
    submitError.value = '请选择仓库';
    return;
  }
  const error = validateStockPolicyLevels(cellForm.min_qty, cellForm.max_qty, cellForm.status);
  if (error) {
    submitError.value = error.message;
    return;
  }
  saving.value = true;
  try {
    await saveStockPolicy(buildStockPolicySavePayload(cellForm));
    MessagePlugin.success(isEdit.value ? '水位已保存' : '水位已创建');
    cellVisible.value = false;
    await reloadList(isEdit.value ? current.value : 1);
  } catch (error) {
    submitError.value = requestErrorMessage(error);
  } finally {
    saving.value = false;
  }
}

// ==================== 批量设置 ====================

const batchVisible = ref(false);
const batchForm = reactive<StockPolicyBatchForm>(emptyStockPolicyBatchForm());
const batchSaving = ref(false);
const batchError = ref('');

/** 实时预览：选了几个物料 × 几个仓库 = 多少格（顺带提示去重 / 超限） */
const batchPreview = computed(() => previewStockPolicyBatch(batchForm.material_ids, batchForm.warehouse_ids));

function openBatch(): void {
  if (!canSave.value) {
    return;
  }
  Object.assign(batchForm, emptyStockPolicyBatchForm());
  batchError.value = '';
  batchVisible.value = true;
}

async function submitBatch(): Promise<void> {
  const error = validateStockPolicyBatch(batchForm, canSave.value);
  batchError.value = error ?? '';
  if (error) {
    return;
  }
  batchSaving.value = true;
  try {
    const result = await saveStockPolicyBatch(buildStockPolicyBatchPayload(batchForm));
    MessagePlugin.success(result.message);
    batchVisible.value = false;
    await reloadList(1);
  } catch (error) {
    batchError.value = requestErrorMessage(error);
  } finally {
    batchSaving.value = false;
  }
}

// ==================== 删除（软删 = 回退物料级） ====================

function confirmDelete(row: StockPolicyRow): void {
  if (!canDelete.value) {
    // 行内链接已按码隐藏（`RowActionDef.permission`），这里再兜一层：拿到 row 对象直接调也不行
    MessagePlugin.warning(`当前账号没有 ${PERM_STOCK_POLICY_DELETE} 权限，无法删除水位设置`);
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除水位设置',
    body: `删除后「${materialText(row)}」在「${warehouseText(row)}」这一格将回退按物料级安全库存判水位（物料级也没设 = 不判）。确定删除？`,
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteStockPolicy(row.id);
        MessagePlugin.success('水位设置已删除（该格回退物料级）');
        dialog.hide();
        await reloadList();
      } catch (error) {
        MessagePlugin.error(requestErrorMessage(error));
      }
    },
  });
}

// ==================== 行内操作 ====================

const actions = computed<ListActions<StockPolicyRow>>(() => ({
  row: [
    {
      key: 'edit',
      label: '编辑',
      permission: PERM_STOCK_POLICY_SAVE,
      testid: 'stock-policy-row-edit',
      handler: (row: StockPolicyRow) => openEdit(row),
    },
    {
      key: 'delete',
      label: '删除',
      theme: 'danger',
      permission: PERM_STOCK_POLICY_DELETE,
      testid: 'stock-policy-row-delete',
      handler: (row: StockPolicyRow) => confirmDelete(row),
    },
  ],
}));

/** 时间展示（后端 `TIMESTAMPTZ` → `yyyy-MM-dd HH:mm`，本地时区；不做时区换算之外的加工） */
function dateTimeText(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  const pad = (n: number): string => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

// ==================== 首屏 ====================

onMounted(async () => {
  await loadMasterData();
});
</script>

<style scoped>
.stock-policy-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.sp-alert {
  margin-bottom: 4px;
}

.sp-gate {
  display: inline-block;
}

.sp-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.sp-cell__main {
  font-weight: 500;
}

.sp-cell__sub {
  font-size: 11px;
  color: var(--td-text-color-secondary);
}

.sp-num {
  font-variant-numeric: tabular-nums;
}

.sp-muted {
  color: var(--td-text-color-placeholder);
}

.sp-mono {
  font-family: var(--td-font-family-mono, monospace);
  font-size: 12px;
}

.sp-empty {
  text-align: center;
  color: var(--td-text-color-placeholder);
  padding: 24px 0;
}

.sp-hint {
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}
</style>
