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
    <!-- ==================== ①② 筛选 / 分页 / 批量 / 行操作交给 ListPage（P3-09） ==================== -->
    <ListPage
      ref="listRef"
      class="tool-row--dense"
      title="工时台账"
      meta-unit="行"
      testid="cl"
      table-testid="cl-table"
      meta-testid="cl-meta"
      btn-prefix="cl-btn"
      :columns="listColumns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :actions="listActions"
      :batch-runner="runBatchAction"
      :page-size="PAGE_SIZE"
      @loaded="onLoaded"
    >
      <template #toolbar>
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

        <t-divider layout="vertical" />

        <span class="tool-row__label tool-row__label--sm">按成本对象分组</span>
        <t-switch
          v-model="grouped"
          data-testid="cl-switch-grouped"
          @change="onGroupedChange"
        />
      </template>

      <template #head-extra>
        <span class="panel-head__tip">
          只有「已确认」的工时会进成本归集；金额 = round(工时 × 工资率, 4)，由系统算
        </span>
      </template>

      <!-- 会计期间：`t-date-picker mode="month"` 不是声明式控件，走 custom 插槽自绘 -->
      <template #filter-period_code>
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
      </template>

      <!-- 深链带进来的成本对象筛选：不在筛选条上，点 × 连其它筛选一起重置（与改造前一致） -->
      <template #filters-extra>
        <t-tag
          v-if="filter.cost_object_id"
          theme="primary"
          variant="light"
          closable
          data-testid="cl-filter-object"
          @close="handleResetAll"
        >
          成本对象 {{ objectFilterNo || shortId(filter.cost_object_id) }}
        </t-tag>
      </template>

      <!-- 分组视图的口径说明（只统计当前页） -->
      <template #filter-footer>
        <t-alert
          v-if="grouped"
          theme="info"
          message="分组汇总只统计「当前页」的行（列表是服务端分页）。要看整期请把筛选的期间缩小到一个月后再翻页核对。"
          class="list-alert"
          data-testid="cl-group-tip"
        />
      </template>

      <!-- ==================== 明细视图 ==================== -->
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

      <!-- ==================== 分组视图（列名带 g_ 前缀，避免与明细同名插槽串台） ==================== -->
      <template #g_cost_object_no="{ row }">
        <span class="mono">{{ row.cost_object_no }}</span>
      </template>
      <template #g_product_name="{ row }">{{ row.product_name || '—' }}</template>
      <template #g_hours_total="{ row }"><span class="num">{{ row.hours_total }}</span></template>
      <template #g_machine_hours_total="{ row }">
        <span class="num">{{ row.machine_hours_total }}</span>
      </template>
      <template #g_amount_total="{ row }">
        <span class="money">{{ formatMoney(row.amount_total) }}</span>
      </template>
      <template #g_progress="{ row }">
        <span class="num">{{ row.confirmed_count }} 已确认 / {{ row.draft_count }} 草稿</span>
      </template>

      <!-- 分组视图的页脚合计（仍在列表卡内，与表格同宽） -->
      <template #footer>
        <p v-if="grouped" class="panel-foot" data-testid="cl-group-foot">
          本页 {{ detailRows.length }} 行 → {{ groups.length }} 个成本对象；人工工时合计
          <b class="num">{{ pageHoursTotal }}</b> 小时，金额合计
          <b class="money">{{ formatMoney(pageAmountTotal) }}</b>
        </p>
      </template>
    </ListPage>

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
import { useRoute, useRouter } from 'vue-router';
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
import ListPage from '@/components/ListPage/index.vue';
import type {
  ColumnDef,
  FilterDef,
  ListActions,
  ListFetcher,
  ListQueryBuilder,
} from '@/components/ListPage/types';
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
  isPeriodKey,
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

// ==================== 列表（列 / 筛选 / 批量 / 拉页交给 ListPage，P3-09） ====================

const route = useRoute();
const router = useRouter();

const filter = reactive<{
  period_code: string;
  status: CostLaborStatus | '';
  dept_id: string;
  keyword: string;
  /**
   * 成本对象筛选（**不在筛选条上**，只由深链 `?cost_object_id=` 带入）：
   * 「未归集清单」点「缺工时的在产对象 / 缺费率的工时」跳进来时用它精确定位那一批，
   * 点筛选条上的对象标签即可清掉（见 `clearObjectFilter`）。
   */
  cost_object_id: string;
}>({ period_code: '', status: '', dept_id: '', keyword: '', cost_object_id: '' });

/**
 * 当前页的**明细**行。
 *
 * 分组视图与页脚合计都从它现算（`groupByObject` / `sumDecimal`），所以它必须存明细、
 * 不是列表当前显示的那些行 —— 分组时列表显示的是聚合结果，拿它算合计会把人数算重。
 */
const detailRows = ref<CostLaborRow[]>([]);
const current = ref(1);
/** 分组视图开关（同一套筛选与分页，只换呈现） */
const grouped = ref(false);

/** 深链带进来的对象号（只用于显示标签，不参与请求） */
const objectFilterNo = ref('');

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

const statusFilterOptions = COST_LABOR_STATUSES.map((value) => ({
  label: statusLabel(value),
  value,
}));

// 列宽合计 ≈ 1112px：1440 视口（侧栏展开，内容区约 1170px）不横向溢出，
// 状态列不会被固定「操作」列盖住（第一版 1260px 实测会被盖）
const columns: ColumnDef[] = [
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
  { colKey: 'op', title: '操作', width: 120, fixed: 'right', exportable: false },
];

/** 分组视图列（`g_` 前缀 = 与明细视图同名的字段换一个插槽，见文件头） */
const groupColumns: ColumnDef[] = [
  { colKey: 'g_cost_object_no', title: '成本对象', width: 170 },
  { colKey: 'g_product_name', title: '产出品', width: 180, ellipsis: true },
  { colKey: 'people_count', title: '人数', width: 70, align: 'right' },
  { colKey: 'row_count', title: '行数', width: 70, align: 'right' },
  { colKey: 'g_hours_total', title: '人工工时', width: 100, align: 'right' },
  { colKey: 'g_machine_hours_total', title: '机器工时', width: 100, align: 'right' },
  { colKey: 'g_amount_total', title: '金额', width: 120, align: 'right' },
  { colKey: 'g_progress', title: '确认进度', width: 160 },
];

const issueColumns: PrimaryTableCol[] = [
  { colKey: 'line', title: '行号', width: 100 },
  { colKey: 'column', title: '列', width: 180 },
  { colKey: 'message', title: '原因' },
];

/** 当前显示用的列：分组视图把同一成本对象的多行合成一行 */
const listColumns = computed<ColumnDef[]>(() => (grouped.value ? groupColumns : columns));

const groups = computed(() => groupByObject(detailRows.value));
const pageHoursTotal = computed(() => sumDecimal(detailRows.value.map((row) => row.hours), 2));
const pageAmountTotal = computed(() =>
  sumDecimal(
    detailRows.value.map((row) => row.amount),
    4,
  ),
);

/**
 * 声明式筛选（顺序即渲染顺序）。
 *
 * 「会计期间」是 `custom`：`t-date-picker mode="month"` 不是声明式控件。
 */
const filters = computed<FilterDef[]>(() => [
  { key: 'period_code', type: 'custom', testid: 'cl-filter-period' },
  {
    key: 'status',
    type: 'select',
    options: statusFilterOptions,
    searchOnChange: true,
    placeholder: '全部状态',
    width: 130,
    testid: 'cl-filter-status',
  },
  {
    key: 'dept_id',
    type: 'select',
    options: deptOptions.value,
    filterable: true,
    searchOnChange: true,
    placeholder: '全部工作中心',
    width: 150,
    testid: 'cl-filter-dept',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '员工 / 对象号 / 产出品（模糊）',
    width: 220,
    testid: 'cl-filter-keyword',
  },
]);

/**
 * 拉一页：先取服务端那一页，再按视图决定喂什么给表格。
 *
 * 分组视图**不是**另一个接口：它把同一页的行聚合后交给表格，`total` 仍是服务端行数，
 * 所以分页器翻的还是明细页（与改造前一致 —— 原来也是「一张表 + 一份 pagination」）。
 */
const fetcher: ListFetcher<CostLaborRow> = async (query) => {
  const data = await fetchCostLaborPage(query as Parameters<typeof fetchCostLaborPage>[0]);
  detailRows.value = data.list ?? [];
  return {
    list: grouped.value ? groupedRows(detailRows.value) : detailRows.value,
    total: Number(data.total ?? 0),
    page: data.page,
    page_size: data.page_size,
  };
};

/**
 * 分组行。
 *
 * 两个要点：
 * 1. `id` 必须补上（= `cost_object_id`）：列表的 `row-key` 始终是 `id`。
 *    切视图时**不能改 row-key**——实测（P3-09）切到分组视图后 t-table 会留下一条
 *    上一版的残行：6 个分组渲染成 7 行，第一组重复一次，刷新也不消失。
 * 2. `g_` 前缀列（`g_cost_object_no` / `g_product_name`）是给分组视图的单元格插槽用的，
 *    原字段名保留（页脚、后续逻辑都按原名读）。
 */
function groupedRows(source: CostLaborRow[]): any[] {
  return groupByObject(source).map((group) => ({
    ...group,
    id: group.cost_object_id,
    g_cost_object_no: group.cost_object_no,
    g_product_name: group.product_name,
  }));
}

/** 入参拼装（与改造前 `loadRows` 里那段逐字一致） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const value = state as {
    period_code: string;
    status: CostLaborStatus | '';
    dept_id: string;
    keyword: string;
    cost_object_id: string;
  };
  return {
    page,
    page_size: pageSize,
    period_code: value.period_code,
    status: value.status,
    dept_id: value.dept_id,
    cost_object_id: value.cost_object_id,
    keyword: value.keyword.trim(),
  };
};

/**
 * 批量动作：分组视图里没有可勾选的行（分组行只用于看汇总），
 * 所以那边不声明批量 —— 否则会出现「勾选框勾不到、批量条一直显示已选 0 条」。
 */
const listActions = computed<ListActions>(() => (grouped.value ? {} : { batch: BATCH_ACTIONS }));

/** 一页拉完：同步页码（明细行已在 `fetcher` 里镜像） */
function onLoaded(payload: { page: number }): void {
  current.value = payload.page;
}

/** 重拉列表（不传 = 当前页）；名字保持不变，页面里 7 处调用点不用改 */
async function loadRows(page?: number): Promise<void> {
  if (page === undefined) {
    await listRef.value?.reload();
    return;
  }
  await listRef.value?.load(page);
}

/** 切分组视图：列与主键换了，重拉同一页即可（筛选与页码不动） */
function onGroupedChange(): void {
  void listRef.value?.reload();
}

/** 筛选项值一变就查（原来 `@change="handleSearch"` 的手感：回第 1 页） */
function handleSearch(): void {
  void listRef.value?.search();
}

/** 重置：清深链带进来的对象筛选 + 声明式筛选（原来 `handleReset` 一并清） */
function handleResetAll(): void {
  clearObjectFilter();
  void listRef.value?.reset();
}

/**
 * 深链 `?period_code=yyyy-MM&cost_object_id=…`（COST-B2-12）：
 * 「未归集清单」的「缺工时的在产对象 / 缺费率的工时」点进来时，直接定位到那一批的工时。
 *
 * 纪律与其它 ERP 页的深链一致：只读 query、只在有值时写筛选（不带参数进页面不能清掉
 * 用户手填的筛选），处理完把这两个 query 摘掉，免得刷新又筛一次。
 *
 * ⚠️ 必须在本组件 setup 里调用（见文件末尾），不能挪到 `onMounted`：ListPage 在**自己的
 * setup** 里就发出首屏请求，那时父组件的 onMounted 还没跑 —— 首屏会先拉一次全量再拉一次。
 */
function applyDeepLink(): void {
  const period = readQuery('period_code');
  const objectId = readQuery('cost_object_id');
  if (isPeriodKey(period)) {
    filter.period_code = period;
  }
  if (objectId) {
    filter.cost_object_id = objectId;
    objectFilterNo.value = readQuery('object_no');
  }
  if (!period && !objectId) {
    return;
  }
  const rest = { ...route.query };
  delete rest.period_code;
  delete rest.cost_object_id;
  delete rest.object_no;
  void router.replace({ path: route.path, query: rest });
}

function readQuery(key: string): string {
  const raw = route.query[key];
  return (typeof raw === 'string' ? raw : Array.isArray(raw) ? String(raw[0] ?? '') : '').trim();
}

/** 清掉深链带进来的对象筛选（筛选条上的标签点 ×） */
function clearObjectFilter(): void {
  filter.cost_object_id = '';
  objectFilterNo.value = '';
}

// 深链必须早于 ListPage 的首屏请求（理由见 `applyDeepLink` 注释）
applyDeepLink();

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

/**
 * 批量确认的执行器（勾选与批量条由 ListPage 接 D7 的 `BatchActionBar`，这里只管逐条调接口）。
 *
 * 行数据从 `detailRows` 取（分组视图下列表显示的是聚合行，不能用它找 `no`）。
 */
async function runBatchAction(_action: BatchActionSpec, ids: string[]): Promise<BatchResult> {
  const succeeded: BatchOkItem[] = [];
  const failed: BatchFailItem[] = [];
  for (const id of ids) {
    const row = detailRows.value.find((item) => item.id === id);
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
  // 列表首屏由 ListPage 自己拉；深链已在 setup 末尾落到 `filter` 上（见 `applyDeepLink`）
  void loadOptions();
});
</script>

<style scoped>
.cost-labor-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
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
