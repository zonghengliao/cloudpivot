<template>
  <div class="prq-page">
    <!-- ==================== ① 筛选 / 工具栏 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="申请单号 / 部门 / 申请人 / PO 号"
          clearable
          data-testid="prq-filter-keyword"
          @enter="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>

        <t-select
          v-model="filter.status"
          class="filter-row__select"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="prq-filter-status"
        />

        <t-date-picker
          v-model="filter.date_from"
          class="filter-row__date"
          value-type="YYYY-MM-DD"
          placeholder="申请日期起"
          clearable
          data-testid="prq-filter-date-from"
        />
        <t-date-picker
          v-model="filter.date_to"
          class="filter-row__date"
          value-type="YYYY-MM-DD"
          placeholder="申请日期止"
          clearable
          data-testid="prq-filter-date-to"
        />

        <t-checkbox v-model="filter.convertible_only" data-testid="prq-filter-convertible">
          只看可转单
        </t-checkbox>

        <t-button theme="primary" data-testid="btn-prq-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-prq-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" data-testid="btn-prq-refresh" @click="loadList">刷新</t-button>

        <span class="filter-row__spacer" />

        <t-tooltip
          :content="
            canCreate
              ? '部门提需求：填物料 / 数量 / 预估单价，提交后走审批，通过后转采购订单'
              : '当前账号没有 business:erp:purchase-requisition:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              :disabled="!canCreate"
              data-testid="btn-prq-new"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建申请
            </t-button>
          </span>
        </t-tooltip>

        <t-tooltip
          :content="
            canToPo
              ? '把选中的已审批申请单**合并**成一张采购订单（同物料数量相加）'
              : '当前账号没有 business:erp:purchase-requisition:to-po 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              variant="outline"
              :disabled="!canToPo || convertibleSelected.length === 0"
              data-testid="btn-prq-merge-to-po"
              @click="openMerge"
            >
              <template #icon><t-icon name="merge-cells" /></template>
              合并转采购订单（{{ convertibleSelected.length }}）
            </t-button>
          </span>
        </t-tooltip>
      </div>

      <t-alert
        v-if="listError"
        theme="error"
        :message="listError"
        class="page-alert"
        data-testid="prq-list-error"
      />
      <t-alert
        v-else-if="mergeHint"
        theme="info"
        :message="mergeHint"
        class="page-alert"
        data-testid="prq-merge-hint"
      />
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="table-card">
      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :row-selection="rowSelection"
        :selected-row-keys="selectedKeys"
        :hover="true"
        size="medium"
        data-testid="prq-table"
        @select-change="onSelectChange"
      >
        <template #req_no="{ row }">
          <span class="mono">{{ row.req_no }}</span>
        </template>
        <template #dept_name="{ row }">
          <div class="cell-stack">
            <span>{{ row.dept_name || '—' }}</span>
            <span class="cell-sub">{{ row.applicant_name || '—' }}</span>
          </div>
        </template>
        <template #est_amount="{ row }">
          <div class="cell-stack cell-right">
            <span class="mono">{{ decimalText(row.est_amount) }}</span>
            <span class="cell-sub">{{ row.item_count }} 行</span>
          </div>
        </template>
        <template #budget="{ row }">
          <t-tag :theme="row.over_budget ? 'danger' : 'default'" variant="light" size="small">
            {{ budgetTextOf(row) }}
          </t-tag>
        </template>
        <template #status="{ row }">
          <t-tag :theme="statusTheme(row.status)" variant="light" size="small">
            {{ row.status_label }}
          </t-tag>
        </template>
        <template #po_no="{ row }">
          <span v-if="row.po_no" class="mono">{{ row.po_no }}</span>
          <span v-else class="cell-sub">—</span>
        </template>
        <template #actions="{ row }">
          <t-space size="small">
            <t-link theme="primary" data-testid="btn-prq-detail" @click="openDetail(row.id)">
              详情
            </t-link>
            <t-link
              v-if="canSubmit(row) && canSubmitPerm"
              theme="warning"
              data-testid="btn-prq-submit"
              @click="handleSubmit(row)"
            >
              提交审批
            </t-link>
            <t-link
              v-if="row.status === 'SUBMITTED'"
              theme="default"
              data-testid="btn-prq-goto-flow"
              @click="goFlowCenter(row)"
            >
              审批中
            </t-link>
            <t-link
              v-if="canConvert(row) && canToPo"
              theme="success"
              data-testid="btn-prq-to-po"
              @click="openToPo(row)"
            >
              转采购单
            </t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="empty-block" data-testid="prq-empty">{{ emptyListText(isFiltered(filter)) }}</div>
        </template>
      </t-table>

      <div class="pager-row">
        <t-pagination
          v-model="page"
          v-model:page-size="pageSize"
          :total="total"
          :page-size-options="[10, 20, 50]"
          show-jumper
          data-testid="prq-pager"
          @change="loadList"
        />
      </div>
    </t-card>

    <!-- ==================== ③ 新建申请 ==================== -->
    <t-dialog
      v-model:visible="createVisible"
      header="新建采购申请"
      width="1000px"
      :confirm-btn="{ content: '保存草稿', loading: creating }"
      :close-on-overlay-click="false"
      data-testid="prq-create-dialog"
      @confirm="handleCreate"
    >
      <t-form label-width="92px" class="create-form">
        <t-row :gutter="[16, 12]">
          <t-col :span="6">
            <t-form-item label="申请部门" required-mark>
              <t-select
                v-model="createForm.dept_id"
                :options="deptOptions"
                placeholder="选择部门"
                filterable
                data-testid="prq-create-dept"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="收货仓库" required-mark>
              <t-select
                v-model="createForm.warehouse_id"
                :options="warehouseOptions"
                placeholder="选择仓库"
                filterable
                data-testid="prq-create-warehouse"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="申请日期">
              <t-date-picker
                v-model="createForm.req_date"
                value-type="YYYY-MM-DD"
                placeholder="缺省 = 今天"
                clearable
                data-testid="prq-create-req-date"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="需求日期">
              <t-date-picker
                v-model="createForm.demand_date"
                value-type="YYYY-MM-DD"
                placeholder="明细没填时用它"
                clearable
                data-testid="prq-create-demand-date"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="备注">
              <t-textarea
                v-model="createForm.remark"
                :maxlength="MAX_REMARK"
                placeholder="为什么需要 / 用途（审批人会看）"
                data-testid="prq-create-remark"
              />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>

      <div class="lines-head">
        <span class="lines-title">申请明细</span>
        <t-button size="small" variant="outline" data-testid="btn-prq-add-line" @click="addLine">
          <template #icon><t-icon name="add" /></template>
          加一行
        </t-button>
      </div>

      <t-table
        row-key="key"
        :data="createForm.lines"
        :columns="lineColumns"
        size="small"
        data-testid="prq-lines-table"
      >
        <template #lineMaterial="{ row }">
          <t-select
            v-model="row.material_id"
            :options="materialOptions"
            placeholder="选择物料"
            filterable
            size="small"
            data-testid="prq-line-material"
          />
        </template>
        <template #lineQty="{ row }">
          <t-input-number
            v-model="row.qty"
            :min="0.0001"
            :decimal-places="4"
            size="small"
            data-testid="prq-line-qty"
          />
        </template>
        <template #linePrice="{ row }">
          <t-input-number
            v-model="row.est_price"
            :min="0"
            :decimal-places="6"
            size="small"
            placeholder="不含税"
            data-testid="prq-line-price"
          />
        </template>
        <template #lineTax="{ row }">
          <t-input-number
            v-model="row.tax_rate"
            :min="0"
            :max="100"
            :decimal-places="4"
            size="small"
            data-testid="prq-line-tax"
          />
        </template>
        <template #lineDemand="{ row }">
          <t-date-picker
            v-model="row.demand_date"
            value-type="YYYY-MM-DD"
            size="small"
            clearable
            placeholder="缺省取单头"
            data-testid="prq-line-demand"
          />
        </template>
        <template #lineSupplier="{ row }">
          <t-select
            v-model="row.suggest_supplier_id"
            :options="supplierOptions"
            placeholder="建议（可空）"
            filterable
            clearable
            size="small"
            data-testid="prq-line-supplier"
          />
        </template>
        <template #lineOps="{ row }">
          <t-link theme="danger" @click="removeLine(row.key)">删除</t-link>
        </template>
      </t-table>
    </t-dialog>

    <!-- ==================== ④ 合并转采购订单 ==================== -->
    <t-dialog
      v-model:visible="toPoVisible"
      :header="toPoTitle"
      width="880px"
      :confirm-btn="{ content: '生成采购订单', loading: converting }"
      :close-on-overlay-click="false"
      data-testid="prq-to-po-dialog"
      @confirm="handleToPo"
    >
      <t-alert
        theme="info"
        class="dialog-alert"
        message="同物料会合并成一行：数量相加、单价按数量加权平均；不同税率的明细不能合并（后端会 409）。"
      />
      <t-form label-width="92px">
        <t-row :gutter="[16, 12]">
          <t-col :span="6">
            <t-form-item label="供应商">
              <t-select
                v-model="toPoForm.supplier_id"
                :options="supplierOptions"
                placeholder="缺省按明细建议供应商推断"
                filterable
                clearable
                data-testid="prq-to-po-supplier"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="下单日期">
              <t-date-picker
                v-model="toPoForm.order_date"
                value-type="YYYY-MM-DD"
                placeholder="缺省 = 今天"
                clearable
                data-testid="prq-to-po-order-date"
              />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="备注">
              <t-input v-model="toPoForm.remark" :maxlength="MAX_REMARK" placeholder="可选" />
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>

      <div class="lines-head">
        <span class="lines-title">合并预览（{{ mergePreview.length }} 行 / {{ mergeDetails.length }} 张申请单）</span>
      </div>
      <t-table
        row-key="material_id"
        :data="mergePreview"
        :columns="mergeColumns"
        size="small"
        data-testid="prq-merge-preview"
      >
        <template #mergeQty="{ row }">
          <span class="mono">{{ decimalText(row.qty) }}</span>
        </template>
        <template #mergePrice="{ row }">
          <span class="mono">{{ decimalText(row.est_price) }}</span>
        </template>
        <template #mergeSources="{ row }">
          <span class="mono">{{ row.source_req_nos.join(' + ') }}</span>
        </template>
      </t-table>
    </t-dialog>

    <!-- ==================== ⑤ 详情 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      :header="detailHeader"
      size="860px"
      :footer="false"
      data-testid="prq-detail-drawer"
    >
      <template v-if="detail">
        <t-descriptions :column="2" size="small" class="detail-desc">
          <t-descriptions-item label="申请单号">{{ detail.requisition.req_no }}</t-descriptions-item>
          <t-descriptions-item label="状态">
            <t-tag :theme="statusTheme(detail.requisition.status)" variant="light" size="small">
              {{ detail.requisition.status_label }}
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="申请部门">{{ detail.requisition.dept_name || '—' }}</t-descriptions-item>
          <t-descriptions-item label="申请人">{{ detail.requisition.applicant_name || '—' }}</t-descriptions-item>
          <t-descriptions-item label="收货仓库">{{ detail.requisition.warehouse_name || '—' }}</t-descriptions-item>
          <t-descriptions-item label="需求日期">{{ detail.requisition.demand_date || '—' }}</t-descriptions-item>
          <t-descriptions-item label="预估金额（不含税）">
            {{ decimalText(detail.requisition.est_amount) }}
          </t-descriptions-item>
          <t-descriptions-item label="预算">
            {{ budgetTextOf(detail.requisition) }}
          </t-descriptions-item>
          <t-descriptions-item label="采购订单">{{ detail.requisition.po_no || '—' }}</t-descriptions-item>
          <t-descriptions-item label="备注">{{ detail.requisition.remark || '—' }}</t-descriptions-item>
        </t-descriptions>

        <div class="lines-head"><span class="lines-title">明细</span></div>
        <t-table
          row-key="id"
          :data="detail.items"
          :columns="detailItemColumns"
          size="small"
          data-testid="prq-detail-items"
        >
          <template #dQty="{ row }">
            <span class="mono">{{ decimalText(row.qty) }}</span>
          </template>
          <template #dPrice="{ row }">
            <span class="mono">{{ decimalText(row.est_price) }}</span>
          </template>
          <template #dPriceIncl="{ row }">
            <span class="mono">{{ decimalText(row.est_price_incl) }}</span>
          </template>
          <template #dAmount="{ row }">
            <span class="mono">{{ decimalText(row.amount) }}</span>
          </template>
          <template #dSupplier="{ row }">
            {{ row.suggest_supplier_name || '—' }}
          </template>
          <template #dPoItem="{ row }">
            <span class="mono cell-sub">{{ row.po_item_id || '—' }}</span>
          </template>
        </t-table>
      </template>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 采购申请页面（P2-02）：需求申请 → 审批 → 转采购订单（**可合并**）。
 *
 * 页面只负责画画与事件；取数 / 组装 / 校验 / 合并口径都在
 * `purchase-requisition-logic.ts`（那里有同后端口径的预校验，用户不用等 409）。
 *
 * 三个必须知道的点：
 * ① 列表勾选只影响「合并转单」——勾了不该转的单（DRAFT / 已转）会在按钮计数里被过滤；
 * ② 提交审批走 `pr_approve` 流程，**超预算那张会自动多一道财务经理**（流程定义里的条件边，
 *    页面不做任何金额判断）；
 * ③ 转单是「合并」语义：路径 id + `requisition_ids` 一起提交，后端按物料合并。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import {
  fetchMaterialPage,
  fetchSupplierOptions,
  fetchWarehousePage,
  type MaterialRow,
  type Supplier,
  type Warehouse,
} from '@/api/business/erp';
import {
  STATUS_OPTIONS,
  convertRequisitionToPo,
  createRequisition,
  decimalText as rowDecimalText,
  fetchRequisitionDetail,
  fetchRequisitionPage,
  statusTheme,
  submitRequisition,
  type RequisitionDetail,
  type RequisitionItem,
  type RequisitionRow,
} from '@/api/business/erp/purchase-requisition';
import {
  MAX_REMARK,
  budgetTextOf,
  buildCreatePayload,
  buildQuery,
  buildToPoPayload,
  canConvert,
  canSubmit,
  dateRangeError,
  decimalText,
  emptyCreateForm,
  emptyFilter,
  emptyListText,
  emptyToPoForm,
  isFiltered,
  newCreateLine,
  validateCreateForm,
  validateMerge,
  type CreateForm,
  type FilterState,
  type ToPoForm,
} from './purchase-requisition-logic';

/** 本页权限码（与 `V0331__purchase_requisition.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  /** 列表 / 详情 */
  list: 'business:erp:purchase-requisition:list',
  /** 新建 */
  create: 'business:erp:purchase-requisition:create',
  /** 提交审批 */
  submit: 'business:erp:purchase-requisition:submit',
  /** 转采购订单 */
  toPo: 'business:erp:purchase-requisition:to-po',
} as const;

const userStore = useUserStore();
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canSubmitPerm = computed(() => userStore.hasPermission(PERM.submit));
const canToPo = computed(() => userStore.hasPermission(PERM.toPo));

/** 错误文案归一（后端 409 的 message 就是给人看的，原样展示） */
function errorText(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    return error.message || fallback;
  }
  if (error instanceof Error) {
    return error.message || fallback;
  }
  return fallback;
}

// ==================== 列表 ====================

const filter = reactive<FilterState>(emptyFilter());
const rows = ref<RequisitionRow[]>([]);
const total = ref(0);
const page = ref(1);
const pageSize = ref(20);
const loading = ref(false);
const listError = ref('');
const mergeHint = ref('');

const statusOptions = STATUS_OPTIONS.map((item) => ({ label: item.label, value: item.value }));

const columns: PrimaryTableCol[] = [
  { colKey: 'req_no', title: '申请单号', width: 180 },
  { colKey: 'req_date', title: '申请日期', width: 120 },
  { colKey: 'dept_name', title: '部门 / 申请人', width: 170 },
  { colKey: 'demand_date', title: '需求日期', width: 120 },
  { colKey: 'est_amount', title: '预估金额', width: 140, align: 'right' },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'budget', title: '预算', width: 190 },
  { colKey: 'po_no', title: '采购订单', width: 170 },
  { colKey: 'actions', title: '操作', width: 220, fixed: 'right' },
];

const selectedKeys = ref<Array<string | number>>([]);

const rowSelection = computed(() => ({
  type: 'checkbox' as const,
  selectedRowKeys: selectedKeys.value,
}));

function onSelectChange(keys: Array<string | number>) {
  selectedKeys.value = keys;
}

const convertibleSelected = computed(() =>
  rows.value.filter((row) => selectedKeys.value.includes(row.id) && canConvert(row)),
);

async function loadList() {
  const rangeError = dateRangeError(filter.date_from, filter.date_to);
  if (rangeError) {
    listError.value = rangeError;
    return;
  }
  loading.value = true;
  listError.value = '';
  try {
    const data = await fetchRequisitionPage(buildQuery(filter, page.value, pageSize.value));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    // 翻页后丢掉不在本页的勾选（否则「合并 3 张」里混进别的页看不见的单）
    const ids = new Set(rows.value.map((row) => row.id));
    selectedKeys.value = selectedKeys.value.filter((key) => ids.has(String(key)));
  } catch (error) {
    listError.value = errorText(error, '加载采购申请失败');
  } finally {
    loading.value = false;
  }
}

function handleSearch() {
  page.value = 1;
  void loadList();
}

function handleReset() {
  Object.assign(filter, emptyFilter());
  page.value = 1;
  void loadList();
}

// ==================== 主数据选项 ====================

const deptOptions = ref<{ label: string; value: string }[]>([]);
const warehouseOptions = ref<{ label: string; value: string }[]>([]);
const materialOptions = ref<{ label: string; value: string }[]>([]);
const supplierOptions = ref<{ label: string; value: string }[]>([]);

function flattenDepts(nodes: DeptNode[], out: { label: string; value: string }[] = []) {
  nodes.forEach((node) => {
    out.push({ label: node.dept_name, value: node.id });
    if (node.children?.length) {
      flattenDepts(node.children, out);
    }
  });
  return out;
}

async function loadOptions() {
  try {
    const [tree, warehouses, materials, suppliers] = await Promise.all([
      getDeptTree({}),
      fetchWarehousePage({ page: 1, page_size: 200, status: 1 }),
      fetchMaterialPage({ page: 1, page_size: 200, status: 1 }),
      fetchSupplierOptions(200),
    ]);
    deptOptions.value = flattenDepts(tree as DeptNode[]);
    warehouseOptions.value = (warehouses.list ?? []).map((item: Warehouse) => ({
      label: item.warehouse_name,
      value: item.id,
    }));
    materialOptions.value = (materials.list ?? []).map((item: MaterialRow) => ({
      label: `${item.material_code} ${item.material_name}`,
      value: item.id,
    }));
    supplierOptions.value = (suppliers as Supplier[]).map((item) => ({
      label: item.supplier_name,
      value: item.id,
    }));
  } catch (error) {
    MessagePlugin.warning(errorText(error, '主数据加载失败（下拉框可能为空）'));
  }
}

// ==================== 新建 ====================

const createVisible = ref(false);
const creating = ref(false);
const createForm = reactive<CreateForm>(emptyCreateForm());

const lineColumns: PrimaryTableCol[] = [
  { colKey: 'material_id', title: '物料', width: 240 },
  { colKey: 'qty', title: '数量', width: 130 },
  { colKey: 'est_price', title: '预估单价', width: 150 },
  { colKey: 'tax_rate', title: '税率%', width: 110 },
  { colKey: 'demand_date', title: '需求日期', width: 160 },
  { colKey: 'suggest_supplier_id', title: '建议供应商', width: 180 },
  { colKey: 'ops', title: '操作', width: 80 },
];

function openCreate() {
  Object.assign(createForm, emptyCreateForm());
  createVisible.value = true;
}

function addLine() {
  createForm.lines.push(newCreateLine());
}

function removeLine(key: number) {
  if (createForm.lines.length <= 1) {
    MessagePlugin.warning('至少保留 1 行明细');
    return;
  }
  createForm.lines = createForm.lines.filter((line) => line.key !== key);
}

async function handleCreate() {
  const error = validateCreateForm(createForm);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  creating.value = true;
  try {
    const detail = await createRequisition(buildCreatePayload(createForm));
    MessagePlugin.success(`已创建 ${detail.requisition.req_no}（草稿）`);
    createVisible.value = false;
    await loadList();
  } catch (err) {
    MessagePlugin.error(errorText(err, '建申请失败'));
  } finally {
    creating.value = false;
  }
}

// ==================== 提交审批 ====================

async function handleSubmit(row: RequisitionRow) {
  try {
    const detail = await submitRequisition(row.id, { reason: '部门提交采购需求' });
    MessagePlugin.success(
      `${detail.requisition.req_no} 已提交审批${
        detail.requisition.over_budget ? '（超预算：已自动加一道财务经理特殊审批）' : ''
      }`,
    );
    await loadList();
  } catch (error) {
    MessagePlugin.error(errorText(error, '提交审批失败'));
  }
}

function goFlowCenter(row: RequisitionRow) {
  MessagePlugin.info(
    `${row.req_no} 在审批中，请到「流程中心 → 我的待办」处理（审批单号见申请详情）`,
  );
}

// ==================== 转采购订单（合并） ====================

const toPoVisible = ref(false);
const converting = ref(false);
const toPoForm = reactive<ToPoForm>(emptyToPoForm());
const mergeDetails = ref<RequisitionDetail[]>([]);

const toPoTitle = computed(() =>
  mergeDetails.value.length > 1
    ? `合并转采购订单（${mergeDetails.value.length} 张申请单）`
    : '转采购订单',
);

/** 合并预览：按物料聚合数量与来源（与后端 merge_by_material 的展示口径一致） */
const mergePreview = computed(() => {
  const byMaterial = new Map<
    string,
    {
      material_id: string;
      material_code: string;
      material_name: string;
      qty: number;
      est_price: string;
      tax_rate: string;
      source_req_nos: string[];
    }
  >();
  mergeDetails.value.forEach((detail) => {
    detail.items.forEach((item: RequisitionItem) => {
      const qty = Number(item.qty);
      const current = byMaterial.get(item.material_id);
      if (current) {
        current.qty += qty;
        if (!current.source_req_nos.includes(detail.requisition.req_no)) {
          current.source_req_nos.push(detail.requisition.req_no);
        }
      } else {
        byMaterial.set(item.material_id, {
          material_id: item.material_id,
          material_code: item.material_code,
          material_name: item.material_name,
          qty,
          est_price: String(item.est_price),
          tax_rate: String(item.tax_rate),
          source_req_nos: [detail.requisition.req_no],
        });
      }
    });
  });
  return [...byMaterial.values()];
});

const mergeColumns: PrimaryTableCol[] = [
  { colKey: 'material_code', title: '物料编码', width: 140 },
  { colKey: 'material_name', title: '物料名称', width: 200 },
  { colKey: 'qty', title: '合并数量', width: 120, align: 'right' },
  { colKey: 'est_price', title: '单价（加权）', width: 130, align: 'right' },
  { colKey: 'tax_rate', title: '税率%', width: 90 },
  { colKey: 'source_req_nos', title: '来源申请单', width: 240 },
];

/** 当前转单的主单 id（路径 id，必须在 requisition_ids 里） */
const toPoAnchor = ref('');

async function openToPo(row: RequisitionRow) {
  await openMergeWith([row.id]);
}

async function openMerge() {
  const ids = convertibleSelected.value.map((row) => row.id);
  if (ids.length === 0) {
    MessagePlugin.warning('请先勾选「已审批」的申请单');
    return;
  }
  await openMergeWith(ids);
}

async function openMergeWith(ids: string[]) {
  try {
    const details = await Promise.all(ids.map((id) => fetchRequisitionDetail(id)));
    if (details.some((detail) => detail.requisition.status !== 'APPROVED')) {
      MessagePlugin.warning('只有「已审批」的申请单可以转采购订单');
      return;
    }
    mergeDetails.value = details;
    toPoAnchor.value = ids[0];
    Object.assign(toPoForm, emptyToPoForm(ids));
    // 建议供应商如果唯一，就预填（省一次点击）
    const suppliers = new Set<string>();
    details.forEach((detail) =>
      detail.items.forEach((item) => {
        if (item.suggest_supplier_id) {
          suppliers.add(item.suggest_supplier_id);
        }
      }),
    );
    if (suppliers.size === 1) {
      toPoForm.supplier_id = [...suppliers][0];
    }
    toPoVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '加载申请单失败'));
  }
}

async function handleToPo() {
  const error = validateMerge(mergeDetails.value, toPoForm.supplier_id);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  converting.value = true;
  try {
    const result = await convertRequisitionToPo(toPoAnchor.value, buildToPoPayload(toPoForm));
    MessagePlugin.success(
      `已生成 ${result.purchase_order.po_no}（${result.merged_count} 张申请单合并为 ${
        result.lines.length
      } 行，价税合计 ${rowDecimalText(result.purchase_order.total_with_tax)}）`,
    );
    toPoVisible.value = false;
    selectedKeys.value = [];
    mergeHint.value = `最近一次合并：${result.requisition_nos.join(' + ')} → ${result.purchase_order.po_no}`;
    await loadList();
  } catch (err) {
    MessagePlugin.error(errorText(err, '转采购订单失败'));
  } finally {
    converting.value = false;
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detail = ref<RequisitionDetail | null>(null);

const detailHeader = computed(() =>
  detail.value ? `${detail.value.requisition.req_no} 采购申请` : '采购申请详情',
);

const detailItemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 60 },
  { colKey: 'material_code', title: '物料编码', width: 140 },
  { colKey: 'material_name', title: '物料名称', width: 180 },
  { colKey: 'unit_name', title: '单位', width: 80 },
  { colKey: 'qty', title: '数量', width: 110, align: 'right' },
  { colKey: 'est_price', title: '预估单价', width: 120, align: 'right' },
  { colKey: 'est_price_incl', title: '含税单价', width: 120, align: 'right' },
  { colKey: 'tax_rate', title: '税率%', width: 90 },
  { colKey: 'amount', title: '行金额', width: 120, align: 'right' },
  { colKey: 'suggest_supplier_id', title: '建议供应商', width: 150 },
  { colKey: 'po_item_id', title: '采购单行', width: 130 },
];

async function openDetail(id: string) {
  try {
    detail.value = await fetchRequisitionDetail(id);
    detailVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error, '加载详情失败'));
  }
}

onMounted(async () => {
  await loadOptions();
  await loadList();
});
</script>

<style scoped>
.prq-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 4px 0 16px;
}

.filter-card,
.table-card {
  padding: 4px 8px;
}

.filter-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.filter-row__keyword {
  width: 240px;
}

.filter-row__select {
  width: 150px;
}

.filter-row__date {
  width: 160px;
}

.filter-row__spacer {
  flex: 1;
}

.gate-wrap {
  display: inline-flex;
}

.page-alert {
  margin-top: 10px;
}

.dialog-alert {
  margin-bottom: 12px;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
}

.cell-stack {
  display: flex;
  flex-direction: column;
  line-height: 1.3;
}

.cell-right {
  align-items: flex-end;
}

.cell-sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.empty-block {
  padding: 24px 0;
  color: var(--td-text-color-placeholder);
}

.pager-row {
  display: flex;
  justify-content: flex-end;
  padding: 12px 4px 0;
}

.lines-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin: 16px 0 8px;
}

.lines-title {
  font-weight: 600;
}

.detail-desc {
  margin-bottom: 8px;
}
</style>
