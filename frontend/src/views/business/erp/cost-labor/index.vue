<!--
  工时台账页（COST-B2-13 / ARCH-9 §6.2.4 · §5.2 #10–#16）。

  一页四件事：**列表（期间/状态/工作中心/关键字）→ 单条录入（费率留空自动取数）→
  批量确认（DRAFT → CONFIRMED）→ xlsx 导入（8 列模板，全有或全无）**。

  # 三条交互口径（都有卡面验收，别弱化）

  ① **按成本对象分组**：车间统计员按批核对。分组表是**当前页**的汇总（后端分页），
     页面必须把"本页合计"写给用户看，否则会被当成当期全量；
  ② **工资率留空 = 后端自动取数**（对象 > 车间 > 公司）：回单里的 `rate_scope_label`
     显示在费率右侧，用户能验收「取到哪一级」；三级全空 ⇒ 422 + 66059，**不按 0 计**；
  ③ **导入全有或全无**：任何一行有错 ⇒ 一条都不入库，逐行红标（行号 + 列 + 原因）。

  # 没有的东西（别以为是漏了）

  * **员工工号**：`biz_employee` 属 ARCH9-2 未上线，单条录入不带 `employee_id`
    （= 按班组计，设计 §4.5.3 允许）；导入里填工号会被后端拒并提示留空。
  * **金额输入框**：金额一律 `round(工时×费率, 4)` 服务端算，页面只显示"预计金额"提示，
    **不提交**（提交了也必须对上，否则 400 + 66058）。
-->
<template>
  <div class="cost-labor-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="cl-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新增工时
        </t-button>
        <t-button
          v-privilege="[PERM.import]"
          variant="outline"
          data-testid="cl-btn-import"
          @click="openImport"
        >
          <template #icon><t-icon name="upload" /></template>
          导入
        </t-button>
        <t-button variant="outline" data-testid="cl-btn-refresh" @click="refresh">刷新</t-button>

        <t-divider layout="vertical" />

        <span class="tool-row__label">按成本对象分组</span>
        <t-switch v-model="grouped" data-testid="cl-switch-grouped" />

        <span class="tool-row__spacer" />

        <t-date-picker
          v-model="filter.period_code"
          class="tool-row__period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="会计期间"
          clearable
          data-testid="cl-filter-period"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="cl-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.dept_id"
          class="tool-row__dept"
          :options="deptOptions"
          placeholder="全部工作中心"
          clearable
          filterable
          data-testid="cl-filter-dept"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="员工 / 对象号 / 产出品（模糊）"
          clearable
          data-testid="cl-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="cl-btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="cl-btn-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">工时台账</span>
          <span class="panel-head__meta" data-testid="cl-meta">共 {{ total }} 行</span>
        </div>
        <span class="panel-head__tip">
          只有「已确认」的工时会进成本归集；金额 = round(工时 × 工资率, 4)，由系统算
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="cl-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadRows">重试</t-button>
        </template>
      </t-alert>

      <!-- D7 批量确认（勾选 → 进度 → 逐条结果） -->
      <BatchActionBar
        :selected="selectedRows"
        :actions="BATCH_ACTIONS"
        :runner="runBatchAction"
        :filtered-total="total"
        testid="cl-batch"
        @update:selected="onSelectedRows"
        @done="handleBatchDone"
      />

      <!-- 分组视图（当前页汇总） -->
      <template v-if="grouped">
        <t-alert
          theme="info"
          message="分组汇总只统计「当前页」的行（列表是服务端分页）。要看整期请把筛选的期间缩小到一个月后再翻页核对。"
          class="list-alert"
          data-testid="cl-group-tip"
        />
        <t-table
          row-key="cost_object_id"
          :data="groups"
          :columns="groupColumns"
          :loading="loading"
          hover
          data-testid="cl-group-table"
        >
          <template #cost_object_no="{ row }">
            <span class="mono">{{ row.cost_object_no }}</span>
          </template>
          <template #product_name="{ row }">{{ row.product_name || '—' }}</template>
          <template #hours_total="{ row }"><span class="num">{{ row.hours_total }}</span></template>
          <template #machine_hours_total="{ row }">
            <span class="num">{{ row.machine_hours_total }}</span>
          </template>
          <template #amount_total="{ row }">
            <span class="money">{{ formatMoney(row.amount_total) }}</span>
          </template>
          <template #progress="{ row }">
            <span class="num">{{ row.confirmed_count }} 已确认 / {{ row.draft_count }} 草稿</span>
          </template>
        </t-table>
        <p class="panel-foot" data-testid="cl-group-foot">
          本页 {{ rows.length }} 行 → {{ groups.length }} 个成本对象；人工工时合计
          <b class="num">{{ pageHoursTotal }}</b> 小时，金额合计
          <b class="money">{{ formatMoney(pageAmountTotal) }}</b>
        </p>
      </template>

      <!-- 明细视图 -->
      <t-table
        v-else
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :selected-row-keys="selectedIds"
        hover
        data-testid="cl-table"
        @select-change="onSelectChange"
        @page-change="handlePageChange"
      >
        <template #work_date="{ row }">
          <span class="mono" :data-testid="`cl-date-${row.id}`">{{ row.work_date }}</span>
        </template>
        <template #cost_object_no="{ row }">
          <div class="name-cell">
            <span class="name-cell__main mono">{{ row.cost_object_no || shortId(row.cost_object_id) }}</span>
            <span class="name-cell__sub">
              {{ row.product_name || '产出品未知' }}
              <t-tag v-if="objectClosed(row)" size="small" variant="light" theme="default">
                {{ row.cost_object_status === 'CLOSED' ? '已关批' : '已作废' }}
              </t-tag>
            </span>
          </div>
        </template>
        <template #employee_name="{ row }">{{ row.employee_name }}</template>
        <template #dept_name="{ row }">{{ row.dept_name || shortId(row.dept_id) }}</template>
        <template #hours="{ row }"><span class="num">{{ row.hours }}</span></template>
        <template #machine_hours="{ row }"><span class="num">{{ row.machine_hours }}</span></template>
        <template #rate="{ row }">
          <span class="num">{{ row.rate }}</span>
          <span v-if="row.rate_scope_label" class="rate-scope">（{{ row.rate_scope_label }}）</span>
        </template>
        <template #amount="{ row }">
          <span class="money" :data-testid="`cl-amount-${row.id}`">{{ formatMoney(row.amount) }}</span>
        </template>
        <template #status="{ row }">
          <t-tag size="small" variant="light" :theme="statusTheme(row.status)">
            {{ statusLabel(row.status) }}
          </t-tag>
        </template>
        <template #source_type="{ row }">
          <span class="muted">{{ sourceLabel(row.source_type) }}</span>
        </template>
        <template #op="{ row }">
          <div class="op-cell">
            <t-link
              v-if="canConfirmRow(row)"
              v-privilege="[PERM.update]"
              theme="primary"
              data-testid="cl-btn-confirm"
              @click="handleConfirm(row)"
            >
              确认
            </t-link>
            <t-link
              v-if="canEditRow(row)"
              v-privilege="[PERM.update]"
              theme="primary"
              data-testid="cl-btn-edit"
              @click="openEdit(row)"
            >
              编辑
            </t-link>
            <t-link
              v-if="canDeleteRow(row)"
              v-privilege="[PERM.delete]"
              theme="danger"
              data-testid="cl-btn-delete"
              @click="handleDelete(row)"
            >
              删除
            </t-link>
            <span v-if="!canEditRow(row)" class="op-cell__hint">已确认只读</span>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 表单（新增 / 编辑） ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑工时（仅草稿）' : '新增工时'"
      width="720px"
      :confirm-btn="{ content: editingId ? '保存' : '提交', loading: submitting }"
      :cancel-btn="{ content: '取消' }"
      data-testid="cl-form-dialog"
      @confirm="submitForm"
      @close="closeForm"
    >
      <t-alert
        v-if="formError"
        theme="error"
        :message="formError"
        class="form-alert"
        data-testid="cl-form-error"
      />
      <t-alert
        v-if="lastRateScopeLabel"
        theme="success"
        :message="`上一层：工资率自动取数命中 ${lastRateScopeLabel}`"
        class="form-alert"
        data-testid="cl-form-rate-scope"
      />

      <t-form label-width="112px" :data="form" data-testid="cl-form">
        <div class="form-grid">
          <t-form-item label="会计期间" :status="formStatus('period_code')" :help="formErrors.period_code">
            <t-date-picker
              v-model="form.period_code"
              mode="month"
              value-type="YYYY-MM"
              format="YYYY-MM"
              placeholder="yyyy-MM"
              data-testid="cl-form-period"
              @change="handleFormDateChange"
            />
          </t-form-item>
          <t-form-item label="工作日期" :status="formStatus('work_date')" :help="formErrors.work_date">
            <t-date-picker
              v-model="form.work_date"
              value-type="YYYY-MM-DD"
              format="YYYY-MM-DD"
              placeholder="YYYY-MM-DD"
              data-testid="cl-form-date"
              @change="handleFormDateChange"
            />
          </t-form-item>
          <t-form-item
            label="成本对象"
            :status="formStatus('cost_object_id')"
            :help="formErrors.cost_object_id"
          >
            <t-select
              v-model="form.cost_object_id"
              :options="objectOptions"
              :loading="optionsLoading"
              placeholder="只列在产（OPEN）对象"
              filterable
              data-testid="cl-form-object"
            />
          </t-form-item>
          <t-form-item label="员工姓名" :status="formStatus('employee_name')" :help="formErrors.employee_name">
            <t-input
              v-model="form.employee_name"
              placeholder="姓名快照（删号后也能回溯）"
              :maxlength="50"
              data-testid="cl-form-employee"
            />
          </t-form-item>
          <t-form-item label="工作中心" :status="formStatus('dept_id')" :help="formErrors.dept_id">
            <DeptTreeSelect v-model="form.dept_id" data-testid="cl-form-dept" />
          </t-form-item>
          <t-form-item label="人工工时" :status="formStatus('hours')" :help="formErrors.hours">
            <t-input
              v-model="form.hours"
              placeholder="> 0，最多 2 位小数"
              data-testid="cl-form-hours"
            />
          </t-form-item>
          <t-form-item
            label="机器工时"
            :status="formStatus('machine_hours')"
            :help="formErrors.machine_hours"
          >
            <t-input
              v-model="form.machine_hours"
              placeholder="可空，缺省 0"
              data-testid="cl-form-machine-hours"
            />
          </t-form-item>
          <t-form-item label="工资率" :status="formStatus('rate')" :help="formErrors.rate">
            <t-input
              v-model="form.rate"
              placeholder="留空 → 自动取数"
              data-testid="cl-form-rate"
            />
          </t-form-item>
        </div>

        <t-form-item label="备注" :status="formStatus('remark')" :help="formErrors.remark">
          <t-input v-model="form.remark" :maxlength="255" placeholder="可空" data-testid="cl-form-remark" />
        </t-form-item>

        <div class="amount-preview" data-testid="cl-form-amount-preview">
          预计金额
          <b class="money">{{ predictedAmount }}</b>
          <span class="muted">
            ＝ round(工时 × 费率, 4)；由服务端计算并落库，页面不提交这个数
          </span>
        </div>
        <p class="field-hint" data-testid="cl-form-rate-hint">{{ RATE_AUTO_HINT }}。{{ RATE_MISSING_HINT }}</p>
      </t-form>
    </t-dialog>

    <!-- ==================== ④ 导入（8 列模板，全有或全无） ==================== -->
    <t-dialog
      v-model:visible="importVisible"
      header="导入工时台账"
      width="760px"
      :footer="false"
      :close-on-overlay-click="false"
      data-testid="cl-import-dialog"
    >
      <t-alert theme="info" class="form-alert">
        模板 8 列：成本对象号 / 日期 / 员工工号 / 员工姓名 / 工作中心（部门）/ 人工工时 /
        机器工时 / 工资率。<b>工资率留空</b>时按「成本对象 &gt; 车间 &gt; 公司」自动取数。
        第 1 行是表头，第 2 行起才是数据；<b>任何一行有错，整批不入库</b>。
      </t-alert>

      <div class="import-row">
        <t-button variant="outline" data-testid="cl-import-template" @click="handleDownloadTemplate">
          <template #icon><t-icon name="download" /></template>
          下载导入模板
        </t-button>
        <t-date-picker
          v-model="importPeriod"
          class="import-row__period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="导入到期间（可选）"
          clearable
          data-testid="cl-import-period"
        />
        <input
          ref="fileInput"
          type="file"
          accept=".xlsx"
          class="hidden-input"
          data-testid="cl-import-file"
          @change="handleFileChange"
        />
        <t-button variant="outline" data-testid="cl-import-pick" @click="pickFile">
          <template #icon><t-icon name="folder" /></template>
          选择 xlsx 文件
        </t-button>
        <span class="import-row__file" data-testid="cl-import-filename">
          {{ importFile ? importFile.name : '未选择文件' }}
        </span>
        <t-button
          theme="primary"
          :loading="importing"
          :disabled="!importFile"
          data-testid="cl-import-submit"
          @click="submitImport"
        >
          上传并校验
        </t-button>
      </div>

      <t-alert
        v-if="importError"
        theme="error"
        :message="importError"
        class="form-alert"
        data-testid="cl-import-error"
      />
      <t-alert
        v-if="importSummary"
        :theme="importFailed ? 'warning' : 'success'"
        :message="importSummary"
        class="form-alert"
        data-testid="cl-import-summary"
      />

      <t-table
        v-if="importIssues.length"
        row-key="line"
        size="small"
        max-height="320"
        :data="importIssues"
        :columns="issueColumns"
        data-testid="cl-import-issues"
      >
        <template #line="{ row }">
          <span class="mono">第 {{ row.line }} 行</span>
        </template>
        <template #message="{ row }">{{ row.message }}</template>
      </t-table>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  confirmCostLabor,
  createCostLabor,
  deleteCostLabor,
  downloadCostLaborTemplate,
  fetchCostLaborPage,
  importCostLabors,
  saveBlob,
  updateCostLabor,
  COST_LABOR_PERMISSIONS,
  COST_LABOR_STATUSES,
  OPTION_LIMIT,
  PAGE_SIZE,
  type CostLaborRow,
  type CostLaborStatus,
  type ImportIssue,
} from '@/api/business/erp/cost-labor';
import { fetchCostObjectPage } from '@/api/business/erp/cost-object';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import BatchActionBar from '@/components/BatchActionBar.vue';
import { useBatchSelection } from '@/composables/useBatchSelection';
import type { BatchActionSpec, BatchFailItem, BatchOkItem, BatchResult } from '@/utils/batch-action';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { formatMoney, shortId, todayString } from '@/views/business/erp/shared/money';
import { errorCodeOf, errorText } from '@/views/business/erp/shared/error-text';
import {
  RATE_AUTO_HINT,
  RATE_MISSING_HINT,
  canConfirmRow,
  canDeleteRow,
  canEditRow,
  emptyLaborForm,
  errorCount,
  expectedAmount,
  formFromRow,
  groupByObject,
  importIssueRows,
  importSummaryText,
  isEmptyPayload,
  objectClosed,
  sourceLabel,
  statusLabel,
  statusTheme,
  sumDecimal,
  toCreatePayload,
  toUpdatePayload,
  validateLaborForm,
  type LaborFormErrors,
  type LaborFormState,
} from './cost-labor-logic';

/** 本页权限码（V0378 种子逐一对应；`confirm` 复用 `update`） */
const PERM = COST_LABOR_PERMISSIONS;

// ==================== 列表 ====================

const rows = ref<CostLaborRow[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');
const grouped = ref(false);

const filter = reactive<{
  period_code: string;
  status: CostLaborStatus | '';
  dept_id: string;
  keyword: string;
}>({ period_code: '', status: '', dept_id: '', keyword: '' });

const statusFilterOptions = COST_LABOR_STATUSES.map((value) => ({
  label: statusLabel(value),
  value,
}));

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// 列宽合计 ≈ 1112px：1440 视口（侧栏展开，内容区约 1170px）不横向溢出，
// 状态列不会被固定「操作」列盖住（第一版 1260px 实测会被盖）
const columns: PrimaryTableCol[] = [
  { colKey: 'work_date', title: '日期', width: 96 },
  { colKey: 'cost_object_no', title: '成本对象 / 产出品', width: 170, ellipsis: true },
  { colKey: 'employee_name', title: '员工', width: 80, ellipsis: true },
  { colKey: 'dept_name', title: '工作中心', width: 96, ellipsis: true },
  { colKey: 'hours', title: '人工工时', width: 90, align: 'right' },
  { colKey: 'machine_hours', title: '机器工时', width: 90, align: 'right' },
  { colKey: 'rate', title: '工资率', width: 110, align: 'right' },
  { colKey: 'amount', title: '金额', width: 100, align: 'right' },
  { colKey: 'status', title: '状态', width: 76 },
  { colKey: 'source_type', title: '来源', width: 84 },
  { colKey: 'op', title: '操作', width: 120, fixed: 'right' },
];

const groupColumns: PrimaryTableCol[] = [
  { colKey: 'cost_object_no', title: '成本对象', width: 170 },
  { colKey: 'product_name', title: '产出品', width: 180, ellipsis: true },
  { colKey: 'people_count', title: '人数', width: 70, align: 'right' },
  { colKey: 'row_count', title: '行数', width: 70, align: 'right' },
  { colKey: 'hours_total', title: '人工工时', width: 100, align: 'right' },
  { colKey: 'machine_hours_total', title: '机器工时', width: 100, align: 'right' },
  { colKey: 'amount_total', title: '金额', width: 120, align: 'right' },
  { colKey: 'progress', title: '确认进度', width: 160 },
];

const issueColumns: PrimaryTableCol[] = [
  { colKey: 'line', title: '行号', width: 100 },
  { colKey: 'column', title: '列', width: 180 },
  { colKey: 'message', title: '原因' },
];

const groups = computed(() => groupByObject(rows.value));
const pageHoursTotal = computed(() => sumDecimal(rows.value.map((row) => row.hours), 2));
const pageAmountTotal = computed(() =>
  sumDecimal(
    rows.value.map((row) => row.amount),
    4,
  ),
);

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const page = await fetchCostLaborPage({
      page: current.value,
      page_size: PAGE_SIZE,
      period_code: filter.period_code,
      status: filter.status,
      dept_id: filter.dept_id,
      keyword: filter.keyword.trim(),
    });
    rows.value = page.list ?? [];
    total.value = Number(page.total ?? 0);
  } catch (error) {
    loadError.value = errorText(error, '加载失败');
    rows.value = [];
    total.value = 0;
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  current.value = 1;
  void loadRows();
}

function handleReset(): void {
  filter.period_code = '';
  filter.status = '';
  filter.dept_id = '';
  filter.keyword = '';
  handleSearch();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadRows();
}

function refresh(): void {
  void loadRows();
}

// ==================== 主数据下拉 ====================

const deptTree = ref<DeptNode[]>([]);
const objectRows = ref<Array<{ id: string; object_no: string; product_name: string | null }>>([]);
const optionsLoading = ref(false);

const deptOptions = computed(() => flattenDeptTree(deptTree.value));

function flattenDeptTree(nodes: DeptNode[], depth = 0): Array<{ label: string; value: string }> {
  const out: Array<{ label: string; value: string }> = [];
  for (const node of nodes) {
    out.push({ label: `${'　'.repeat(depth)}${node.dept_name}`, value: node.id });
    const children = node.children ?? [];
    if (children.length) {
      out.push(...flattenDeptTree(children, depth + 1));
    }
  }
  return out;
}

/**
 * 成本对象下拉：只列 `OPEN`（报工只能报到在产对象上）。
 *
 * ⚠️ 这里查的是 `cost-object` 模块的列表接口（权限 `cost-object:list`）。财务角色
 * 由 V0378 同时授予两页权限，所以不存在 403；若将来给「只录工时」的角色，需要
 * 一个最小投影接口（与出库单 `cost-object-options` 同款，见 skill 的前端卡配方）。
 */
const objectOptions = computed(() =>
  objectRows.value.map((row) => ({
    label: `${row.object_no} · ${row.product_name ?? '产出品未知'}`,
    value: row.id,
  })),
);

async function loadOptions(): Promise<void> {
  optionsLoading.value = true;
  try {
    const [tree, page] = await Promise.all([
      getDeptTree({ status: 1 }),
      fetchCostObjectPage({ status: 'OPEN', page: 1, page_size: OPTION_LIMIT }),
    ]);
    deptTree.value = tree;
    objectRows.value = page.list ?? [];
  } catch (error) {
    MessagePlugin.warning(errorText(error, '下拉主数据加载失败'));
  } finally {
    optionsLoading.value = false;
  }
}

// ==================== 表单（新增 / 编辑） ====================

const formVisible = ref(false);
const submitting = ref(false);
const formError = ref('');
const formErrors = ref<LaborFormErrors>({});
const editingId = ref('');
const editingOrigin = ref<CostLaborRow | null>(null);
const form = reactive<LaborFormState>(emptyLaborForm(todayString()));
const lastRateScopeLabel = ref('');

function formStatus(field: keyof LaborFormState): 'error' | undefined {
  return formErrors.value[field] ? 'error' : undefined;
}

/** 期间跟着工作日期走（后端要求日期落在期间内；用户改了日期不跟着改期间是最常见的 400） */
function handleFormDateChange(): void {
  if (form.work_date.length === 7 && form.period_code !== form.work_date) {
    form.period_code = form.work_date;
  }
}

/** 预计金额（纯提示；不提交） */
const predictedAmount = computed(() => {
  const rate = form.rate.trim();
  if (!form.hours.trim() || !rate) {
    return '—';
  }
  return expectedAmount(form.hours.trim(), rate);
});

function resetFormState(): void {
  Object.assign(form, emptyLaborForm(todayString()));
  formErrors.value = {};
  formError.value = '';
  editingOrigin.value = null;
  lastRateScopeLabel.value = '';
}

function openCreate(): void {
  editingId.value = '';
  resetFormState();
  formVisible.value = true;
}

function openEdit(row: CostLaborRow): void {
  editingId.value = row.id;
  resetFormState();
  editingOrigin.value = row;
  Object.assign(form, formFromRow(row));
  formVisible.value = true;
}

function closeForm(): void {
  formVisible.value = false;
  resetFormState();
  editingId.value = '';
}

async function submitForm(): Promise<void> {
  formError.value = '';
  const errors = validateLaborForm(form);
  formErrors.value = errors;
  if (errorCount(errors) > 0) {
    formError.value = '还有必填项没填对，请看下方标红项';
    return;
  }
  submitting.value = true;
  try {
    if (editingId.value) {
      const origin = editingOrigin.value;
      if (!origin) {
        formError.value = '原行没加载成功，请关闭后重试';
        return;
      }
      const payload = toUpdatePayload(origin, form);
      if (isEmptyPayload(payload)) {
        MessagePlugin.info('没有改动');
        return;
      }
      const saved = await updateCostLabor(editingId.value, payload);
      MessagePlugin.success(`已保存 ${saved.work_date} ${saved.employee_name}`);
      closeForm();
    } else {
      const created = await createCostLabor(toCreatePayload(form));
      lastRateScopeLabel.value = created.rate_scope_label ?? '';
      MessagePlugin.success(
        created.rate_scope_label
          ? `已录入：金额 ${created.amount}（工资率取自 ${created.rate_scope_label}）`
          : `已录入：金额 ${created.amount}`,
      );
      // 建单后保留对话框会把"取数来源"显示在字段旁供用户验收（§6.2.4），再关
      formVisible.value = false;
      resetFormState();
      editingId.value = '';
      lastRateScopeLabel.value = created.rate_scope_label ?? '';
    }
    await loadRows();
  } catch (error) {
    // 66059（三级费率全空）/ 66058（金额不符）的报文里已有"怎么修"，原样展示
    const code = errorCodeOf(error);
    formError.value =
      code === 66059 ? `${errorText(error)}\n\n${RATE_MISSING_HINT}` : errorText(error);
  } finally {
    submitting.value = false;
  }
}

// ==================== 行操作（确认 / 删除） ====================

async function handleConfirm(row: CostLaborRow): Promise<void> {
  try {
    const confirmed = await confirmCostLabor(row.id);
    MessagePlugin.success(`已确认 ${confirmed.work_date} ${confirmed.employee_name}（${confirmed.amount} 元）`);
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error, '确认失败'));
  }
}

async function handleDelete(row: CostLaborRow): Promise<void> {
  const confirmed = await new Promise<boolean>((resolve) => {
    const dialog = DialogPlugin.confirm({
      header: '删除工时',
      body: `删除 ${row.work_date} ${row.employee_name} 的 ${row.hours} 小时工时？草稿才可删，已确认的行请先作废重录。`,
      theme: 'warning',
      confirmBtn: '删除',
      onConfirm: () => {
        dialog.hide();
        resolve(true);
      },
      onClose: () => resolve(false),
      onCancel: () => resolve(false),
    });
  });
  if (!confirmed) {
    return;
  }
  try {
    await deleteCostLabor(row.id);
    MessagePlugin.success('已删除');
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error, '删除失败'));
  }
}

// ==================== 批量确认 ====================

const BATCH_ACTIONS: BatchActionSpec[] = [
  {
    key: 'confirm',
    label: '批量确认',
    theme: 'primary',
    allowedStatus: ['DRAFT'],
    targetStatus: 'CONFIRMED',
    needConfirm: true,
  },
];

const {
  selectedIds,
  selectedRows,
  onSelectChange,
  onSelectedRows,
} = useBatchSelection(BATCH_ACTIONS, rows);

async function runBatchAction(_action: BatchActionSpec, ids: string[]): Promise<BatchResult> {
  const succeeded: BatchOkItem[] = [];
  const failed: BatchFailItem[] = [];
  for (const id of ids) {
    const row = rows.value.find((item) => item.id === id);
    try {
      await confirmCostLabor(id);
      succeeded.push({ id, no: row?.cost_object_no ?? null });
    } catch (error) {
      failed.push({
        id,
        no: row?.cost_object_no ?? null,
        code: errorCodeOf(error) ?? 0,
        error: errorText(error),
      });
    }
  }
  return {
    succeeded,
    failed,
    total: ids.length,
    success_count: succeeded.length,
    fail_count: failed.length,
  };
}

function handleBatchDone(): void {
  void loadRows();
}

// ==================== 导入 ====================

const importVisible = ref(false);
const importing = ref(false);
const importError = ref('');
const importSummary = ref('');
const importFailed = ref(false);
const importIssues = ref<ImportIssue[]>([]);
const importPeriod = ref('');
const importFile = ref<File | null>(null);
const fileInput = ref<HTMLInputElement | null>(null);

function openImport(): void {
  importVisible.value = true;
  importError.value = '';
  importSummary.value = '';
  importFailed.value = false;
  importIssues.value = [];
  importFile.value = null;
  importPeriod.value = filter.period_code;
}

function pickFile(): void {
  fileInput.value?.click();
}

function handleFileChange(event: Event): void {
  const input = event.target as HTMLInputElement;
  importFile.value = input.files?.[0] ?? null;
  importError.value = '';
  importSummary.value = '';
  importIssues.value = [];
  // 同一文件再选一次也要能触发 change
  input.value = '';
}

async function handleDownloadTemplate(): Promise<void> {
  try {
    const blob = await downloadCostLaborTemplate();
    saveBlob(blob, '工时台账导入模板.xlsx');
  } catch (error) {
    importError.value = errorText(error, '模板下载失败');
  }
}

async function submitImport(): Promise<void> {
  const file = importFile.value;
  if (!file) {
    importError.value = '请先选择 xlsx 文件';
    return;
  }
  importing.value = true;
  importError.value = '';
  importSummary.value = '';
  importIssues.value = [];
  try {
    const result = await importCostLabors(file, importPeriod.value || undefined);
    importFailed.value = result.failed > 0;
    importSummary.value = importSummaryText(result);
    importIssues.value = importIssueRows(result.rows);
    if (result.success > 0) {
      MessagePlugin.success(`导入成功 ${result.success} 行`);
      await loadRows();
    }
  } catch (error) {
    importError.value = errorText(error, '导入失败');
  } finally {
    importing.value = false;
  }
}

onMounted(() => {
  void loadOptions();
  void loadRows();
});
</script>

<style scoped>
.cost-labor-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-card,
.list-card {
  border-radius: 8px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.tool-row__label {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.tool-row__spacer {
  flex: 1 1 auto;
}

.tool-row__period {
  width: 132px;
}

.tool-row__status {
  width: 118px;
}

.tool-row__dept {
  width: 156px;
}

.tool-row__keyword {
  width: 220px;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 8px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.panel-head__title {
  font-size: 16px;
  font-weight: 600;
}

.panel-head__meta {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.panel-head__tip {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.panel-foot {
  margin: 8px 0 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.list-alert,
.form-alert {
  margin-bottom: 8px;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12px;
}

.name-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.name-cell__main {
  font-weight: 500;
}

.name-cell__sub {
  display: flex;
  align-items: center;
  gap: 4px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.num,
.money {
  font-variant-numeric: tabular-nums;
}

.rate-scope {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.muted {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.op-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.op-cell__hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 0 12px;
}

.field-hint {
  margin: 4px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 1.5;
}

.amount-preview {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 8px 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
  font-size: 12px;
}

.import-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin-bottom: 8px;
}

.import-row__period {
  width: 150px;
}

.import-row__file {
  flex: 1 1 auto;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  word-break: break-all;
}

.hidden-input {
  display: none;
}
</style>
