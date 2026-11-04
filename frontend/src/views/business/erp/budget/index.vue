<!--
  预算编制页（FIN-1-FE / ARCH-6 第三章）。

  这一页只做「编排 + 展示」，口径全部来自 `budget-logic.ts`（被单测钉住）：
  ① 12 个月分解**必须** Σ = 年度额度，尾差只能记 12 月（后端 `validate_periods` 同口径）；
  ② 动作按钮按状态给：`ACTIVE` 只给「调整」不给「编辑」（ARCH-6 硬规则 4，
     已生效版本改额度 = 必 409 的按钮）；
  ③ 提交后状态只能靠重新拉列表/详情，前端不猜状态机（`next_statuses` 由后端给）。

  手工注册路由见 `src/router/index.ts`（没有 views 自动扫描，漏了 = 点菜单 404）。
-->
<template>
  <div class="budget-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="budget-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建预算
        </t-button>
        <t-button variant="outline" data-testid="budget-btn-refresh" @click="refresh">刷新</t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="yearFilterValue"
          class="tool-row__year"
          :options="yearOptions"
          placeholder="全部年度"
          clearable
          data-testid="budget-filter-year"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.dept_id"
          class="tool-row__dept"
          :options="deptOptions"
          placeholder="全部部门"
          clearable
          filterable
          data-testid="budget-filter-dept"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="budget-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.change_type"
          class="tool-row__change"
          :options="changeTypeFilterOptions"
          placeholder="编制 / 调整"
          clearable
          data-testid="budget-filter-change"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="单号 / 科目 / 部门（模糊）"
          clearable
          data-testid="budget-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="budget-btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="budget-btn-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 预算清单 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">预算编制</span>
          <span class="panel-head__meta" data-testid="budget-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">
          一条预算 = 部门 × 预算科目 × 年度；同槽同时只有一个「已生效」版本，改额度走「调整」
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="budget-load-error"
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
        data-testid="budget-table"
        @page-change="handlePageChange"
      >
        <template #budget_no="{ row }">
          <span class="mono" :data-testid="`budget-no-${row.budget_no}`">{{ row.budget_no }}</span>
        </template>
        <template #dept_name="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.dept_name }}</span>
            <span class="name-cell__sub">{{ row.fiscal_year }} 年度</span>
          </div>
        </template>
        <template #account="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.account_name }}</span>
            <span class="name-cell__sub">{{ row.account_code }}</span>
          </div>
        </template>
        <template #total_amount="{ row }">
          <span class="money money--strong">{{ formatMoney(row.total_amount) }}</span>
        </template>
        <template #control_mode="{ row }">
          <span class="muted">{{ controlModeShort(row.control_mode) }}</span>
        </template>
        <template #version_no="{ row }">
          <div class="version-cell">
            <span class="mono">v{{ row.version_no }}</span>
            <t-tag size="small" variant="light" :theme="changeTypeTheme(row.change_type)">
              {{ changeTypeLabel(row.change_type) }}
            </t-tag>
          </div>
        </template>
        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="budgetStatusTheme(row.status)"
            :data-testid="`budget-status-${row.budget_no}`"
          >
            {{ budgetStatusLabel(row.status, row.status_label) }}
          </t-tag>
        </template>
        <template #op="{ row }">
          <div class="op-cell">
            <t-link
              v-for="action in budgetRowActions(row, permissions)"
              :key="action.key"
              :theme="action.danger ? 'danger' : 'primary'"
              :data-testid="`budget-btn-${action.key}-${row.budget_no}`"
              @click="handleAction(action.key, row)"
            >
              {{ action.label }}
            </t-link>
            <!-- 草稿但没有提交权限：显式说明为什么按钮不见了，而不是让人反复点 -->
            <t-tooltip
              v-if="row.status === 'DRAFT' && !permissions.submit"
              content="当前角色没有「预算提交」权限（business:erp:budget:submit）"
            >
              <span class="muted muted--hint" :data-testid="`budget-btn-submit-denied-${row.budget_no}`">
                提交?
              </span>
            </t-tooltip>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 编制 / 编辑 / 调整弹窗 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="formTitle"
      width="1080px"
      :confirm-btn="{ content: formConfirmText, loading: submitting }"
      :cancel-btn="{ content: '取消' }"
      :close-on-overlay-click="!submitting"
      data-testid="budget-form-dialog"
      @confirm="submitForm"
      @close="closeForm"
    >
      <div class="form-layout">
        <div class="form-layout__main">
          <t-form :data="form" label-width="104px" colon>
            <t-row :gutter="[16, 0]">
              <t-col :span="6">
                <t-form-item label="年度">
                  <t-select
                    v-model="form.fiscal_year"
                    :options="yearOptions"
                    :disabled="formMode !== 'create'"
                    data-testid="budget-form-year"
                    @change="handleYearChange"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="6">
                <t-form-item label="部门" :help="fieldError('dept_id')">
                  <t-select
                    v-model="form.dept_id"
                    :options="deptOptions"
                    placeholder="选择部门"
                    filterable
                    :disabled="formMode !== 'create'"
                    data-testid="budget-form-dept"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item label="预算科目" :help="fieldError('account_id')">
                  <t-select
                    v-model="form.account_id"
                    :options="accountOptions"
                    placeholder="选择末级预算科目（父科目不可选）"
                    filterable
                    :disabled="formMode !== 'create'"
                    data-testid="budget-form-account"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="6">
                <t-form-item label="年度额度">
                  <t-input
                    v-model="form.total_amount"
                    placeholder="0.00"
                    suffix="元"
                    data-testid="budget-form-total"
                    @blur="applyEvenSplitSilently"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="6">
                <t-form-item label="控制策略">
                  <t-select
                    v-model="form.control_mode"
                    :options="controlModeOptions"
                    data-testid="budget-form-control"
                  />
                </t-form-item>
              </t-col>
              <!-- 调整：原因必填（后端空原因直接 400） -->
              <t-col v-if="formMode === 'adjust'" :span="12">
                <t-form-item label="调整原因" :help="fieldError('change_reason')">
                  <t-textarea
                    v-model="changeReason"
                    placeholder="为什么要改额度？（审计要能说清，最长 500 字）"
                    :maxlength="MAX_CHANGE_REASON"
                    :autosize="{ minRows: 2, maxRows: 4 }"
                    data-testid="budget-form-reason"
                  />
                </t-form-item>
              </t-col>
              <t-col :span="12">
                <t-form-item label="备注">
                  <t-input
                    v-model="form.remark"
                    placeholder="可选（最长 2000 字）"
                    :maxlength="MAX_REMARK"
                    data-testid="budget-form-remark"
                  />
                </t-form-item>
              </t-col>
            </t-row>
          </t-form>

          <!-- 12 个月分解：Σ 必须 = 年度额度 -->
          <div class="month-block">
            <div class="month-block__head">
              <span class="month-block__title">月度分解（12 个月）</span>
              <span class="month-block__tip">12 个月合计必须等于年度额度，尾差记到 12 月</span>
            </div>
            <div class="month-grid">
              <div v-for="(row, index) in form.rows" :key="row.month" class="month-grid__cell">
                <span class="month-grid__label">{{ row.month }} 月</span>
                <t-input
                  v-model="row.amount"
                  size="small"
                  placeholder="0.00"
                  :data-testid="`budget-month-${row.month}`"
                  @blur="touchMonth(index)"
                />
              </div>
            </div>
            <div class="month-foot">
              <div class="month-foot__sums">
                <span>
                  合计
                  <b :class="{ 'sum-bad': !monthCheck.ok }" data-testid="budget-month-sum">{{ monthSum }}</b>
                  / 额度 {{ formatMoney(form.total_amount) }}
                </span>
                <span v-if="monthDiffText" class="sum-bad" data-testid="budget-month-diff">
                  差额 {{ monthDiffText }}
                </span>
              </div>
              <div class="month-foot__actions">
                <t-button size="small" variant="outline" data-testid="budget-btn-even" @click="applyEvenSplit">
                  平均分摊
                </t-button>
                <t-button size="small" variant="outline" data-testid="budget-btn-tail" @click="applyTail">
                  尾差记 12 月
                </t-button>
                <t-button size="small" variant="outline" data-testid="budget-btn-prior" @click="applyPrior">
                  按上年占比
                </t-button>
              </div>
            </div>
          </div>

          <t-alert
            v-if="formErrors.length"
            theme="error"
            class="form-alert"
            data-testid="budget-form-errors"
          >
            <div v-for="(item, index) in formErrors" :key="index">{{ item }}</div>
          </t-alert>
        </div>

        <!-- 右栏：实时口径 -->
        <aside class="form-side">
          <div class="form-side__title">提交前自检</div>
          <ul class="form-side__list">
            <li :class="{ 'is-bad': !form.dept_id }">已选部门{{ form.dept_id ? '' : '（必填）' }}</li>
            <li :class="{ 'is-bad': !form.account_id }">已选末级科目{{ form.account_id ? '' : '（必填）' }}</li>
            <li :class="{ 'is-bad': !monthCheck.ok }">
              12 月合计 = 年度额度{{ monthCheck.ok ? ' ✓' : '（未配平）' }}
            </li>
            <li>控制策略：{{ controlModeText }}</li>
          </ul>
          <div class="form-side__note">
            保存后状态为「草稿」，提交审批通过才生效（ACTIVE）；已生效的预算只能「调整」生成新版本，
            旧版本自动标记为「已被取代」。
          </div>
        </aside>
      </div>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="760px"
      :footer="false"
      :header="false"
      data-testid="budget-drawer"
    >
      <div v-if="detailLoading" class="drawer-loading">加载中…</div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" data-testid="budget-detail-error" />
      <div v-else-if="detail" class="drawer" data-testid="budget-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title mono">{{ detail.budget.budget_no }}</span>
            <t-tag size="small" variant="light" :theme="budgetStatusTheme(detail.budget.status)">
              {{ budgetStatusLabel(detail.budget.status, detail.budget.status_label) }}
            </t-tag>
            <t-tag size="small" variant="light" :theme="changeTypeTheme(detail.budget.change_type)">
              {{ changeTypeLabel(detail.budget.change_type) }} v{{ detail.budget.version_no }}
            </t-tag>
          </div>
          <t-button size="small" variant="outline" @click="refreshDetail">刷新</t-button>
        </div>

        <div class="drawer-usage">
          <div class="drawer-usage__label">
            <span>年度已用 {{ formatMoney(detail.year_used) }}</span>
            <span>可用 {{ formatMoney(detail.year_available) }}</span>
          </div>
          <t-progress :percentage="detailUsagePercent(detail)" :label="false" theme="line" />
        </div>

        <t-descriptions :column="2" size="small" bordered>
          <t-descriptions-item label="年度">{{ detail.budget.fiscal_year }}</t-descriptions-item>
          <t-descriptions-item label="部门">{{ detail.budget.dept_name }}</t-descriptions-item>
          <t-descriptions-item label="预算科目">
            {{ detail.budget.account_name }}（{{ detail.budget.account_code }}）
          </t-descriptions-item>
          <t-descriptions-item label="年度额度">
            <span class="money money--strong">{{ formatMoney(detail.budget.total_amount) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="控制策略">{{ controlModeLabel(detail.budget.control_mode) }}</t-descriptions-item>
          <t-descriptions-item label="提交时间">{{ dateTimeText(detail.budget.submitted_at) }}</t-descriptions-item>
          <t-descriptions-item label="通过时间">{{ dateTimeText(detail.budget.approved_at) }}</t-descriptions-item>
          <t-descriptions-item label="备注">{{ dash(detail.budget.remark) }}</t-descriptions-item>
        </t-descriptions>

        <div class="drawer-section">
          <div class="drawer-section__title">月度分解</div>
          <t-table
            row-key="period_code"
            size="small"
            :data="detail.periods"
            :columns="periodColumns"

          >
            <template #amount="{ row }">
              <span class="money">{{ formatMoney(row.amount) }}</span>
            </template>
            <template #used="{ row }">
              <span class="money">{{ formatMoney(row.used) }}</span>
            </template>
            <template #available="{ row }">
              <span class="money" :class="{ 'money--bad': isMonthOver(row) }">
                {{ formatMoney(row.available) }}
              </span>
            </template>
            <template #remark="{ row }">
              <span class="muted">{{ dash(row.remark) }}</span>
            </template>
          </t-table>
        </div>

        <div class="drawer-section">
          <div class="drawer-section__title">
            占用 / 执行记录
            <span class="muted">（超预算标记是提交那一刻的快照，额度后来调高也不会变）</span>
          </div>
          <t-table
            v-if="detail.usages.length"
            row-key="id"
            size="small"
            :data="detail.usages"
            :columns="usageColumns"

          >
            <template #amount="{ row }">
              <span class="money">{{ formatMoney(row.amount) }}</span>
            </template>
            <template #status="{ row }">
              <t-tag size="small" variant="light" :theme="usageStatusTheme(row.status)">
                {{ row.status_label || row.status }}
              </t-tag>
            </template>
            <template #over="{ row }">
              <span v-if="row.over_budget" class="money--bad">{{ usageOverText(row) }}</span>
              <span v-else class="muted">—</span>
            </template>
          </t-table>
          <div v-else class="drawer-empty">还没有占用或执行记录（采购单提交才会产生占用）</div>
        </div>

        <div class="drawer-section">
          <div class="drawer-section__title">版本链</div>
          <t-table
            row-key="id"
            size="small"
            :data="detail.versions"
            :columns="versionColumns"

          >
            <template #total_amount="{ row }">
              <span class="money">{{ formatMoney(row.total_amount) }}</span>
            </template>
            <template #status="{ row }">
              <t-tag size="small" variant="light" :theme="budgetStatusTheme(row.status)">
                {{ budgetStatusLabel(row.status, row.status_label) }}
              </t-tag>
            </template>
            <template #created_at="{ row }">
              <span class="muted">{{ dateTimeText(row.created_at) }}</span>
            </template>
          </t-table>
        </div>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  adjustBudget,
  createBudget,
  deleteBudget,
  fetchBudgetAccountOptions,
  fetchBudgetDetail,
  fetchBudgetPage,
  submitBudget,
  updateBudget,
  type BudgetDetail,
  type BudgetRow,
  type CreateBudgetReq,
} from '@/api/business/erp/budget';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { useUserStore } from '@/stores/user';
import { dash, formatMoney, toCents } from '@/views/business/erp/shared/money';
import {
  BUDGET_PERMS,
  BUDGET_STATUS_OPTIONS,
  CHANGE_TYPE_OPTIONS,
  CONTROL_MODE_OPTIONS,
  MAX_CHANGE_REASON,
  MAX_REMARK,
  applyTailToDecember,
  budgetFilterParams,
  budgetRowActions,
  budgetStatusLabel,
  budgetStatusTheme,
  centsToText,
  changeTypeLabel,
  changeTypeTheme,
  checkBudgetForm,
  checkMonthDraft,
  controlModeLabel,
  detailUsagePercent,
  emptyBudgetFilter,
  emptyBudgetForm,
  evenSplitRows,
  fiscalYearOptions,
  isMonthOver,
  monthRowsFromPeriods,
  priorRatioRows,
  reyearRows,
  sumMonthText,
  toAdjustPayload,
  toCreatePayload,
  toUpdatePayload,
  usageOverText,
  usageStatusTheme,
  type BudgetActionKey,
  type BudgetFilterState,
  type BudgetFormState,
  type BudgetPermissions,
} from './budget-logic';

/** 权限码（与 V0227 种子逐一对应） */
const PERM = BUDGET_PERMS;

/** 每页条数（后端 `DEFAULT_PAGE_SIZE`） */
const PAGE_SIZE = 20;
/** 下拉一次拉多少 */
const OPTION_LIMIT = 300;

// ==================== 列表 ====================

const rows = ref<BudgetRow[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');

const filter = reactive<BudgetFilterState>(emptyBudgetFilter());

/**
 * 年度筛选的 v-model 代理。
 *
 * `BudgetFilterState.fiscal_year` 是 `number | null`（与后端 `Option<i16>` 同构），
 * 而 `t-select` 的 `modelValue` 不接受 `null`（typecheck 会红）。这里把 `null` 与
 * `undefined` 互转，逻辑层口径不变。
 */
const yearFilterValue = computed<number | undefined>({
  get: () => filter.fiscal_year ?? undefined,
  set: (value) => {
    filter.fiscal_year = typeof value === 'number' ? value : null;
  },
});

const yearOptions = fiscalYearOptions().map((year) => ({ label: `${year} 年度`, value: year }));
const statusFilterOptions = BUDGET_STATUS_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const changeTypeFilterOptions = CHANGE_TYPE_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const controlModeOptions = CONTROL_MODE_OPTIONS.map((item) => ({ label: item.label, value: item.value }));

const depts = ref<DeptNode[]>([]);
const accounts = ref<Array<{ id: string; account_code: string; account_name: string }>>([]);

/** 部门树拍平成下拉（缩进表示层级） */
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
  accounts.value.map((item) => ({
    label: `${item.account_code} ${item.account_name}`,
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
  { colKey: 'budget_no', title: '预算单号', width: 158 },
  { colKey: 'dept_name', title: '部门 / 年度', width: 126, ellipsis: true },
  { colKey: 'account', title: '预算科目', minWidth: 160, ellipsis: true },
  { colKey: 'total_amount', title: '年度额度', width: 112, align: 'right' },
  { colKey: 'control_mode', title: '策略', width: 82 },
  { colKey: 'version_no', title: '版本 / 类型', width: 104 },
  { colKey: 'status', title: '状态', width: 84 },
  { colKey: 'op', title: '操作', width: 196, fixed: 'right' },
];

const periodColumns: PrimaryTableCol[] = [
  { colKey: 'period_code', title: '期间', width: 92 },
  { colKey: 'amount', title: '额度', width: 120, align: 'right' },
  { colKey: 'used', title: '已用', width: 120, align: 'right' },
  { colKey: 'available', title: '可用', width: 120, align: 'right' },
  { colKey: 'remark', title: '备注', ellipsis: true },
];

const usageColumns: PrimaryTableCol[] = [
  { colKey: 'source_no', title: '来源单号', width: 180 },
  { colKey: 'period_code', title: '期间', width: 92 },
  { colKey: 'amount', title: '金额', width: 120, align: 'right' },
  { colKey: 'status', title: '状态', width: 96 },
  { colKey: 'over', title: '超预算', width: 150 },
];

const versionColumns: PrimaryTableCol[] = [
  { colKey: 'version_no', title: '版本', width: 70 },
  { colKey: 'budget_no', title: '单号', width: 168 },
  { colKey: 'total_amount', title: '年度额度', width: 130, align: 'right' },
  { colKey: 'status', title: '状态', width: 96 },
  { colKey: 'created_at', title: '创建时间', ellipsis: true },
];

// ==================== 权限 ====================

const userStore = useUserStore();

const permissions = computed<BudgetPermissions>(() => ({
  create: userStore.hasPermission(BUDGET_PERMS.create),
  update: userStore.hasPermission(BUDGET_PERMS.update),
  delete: userStore.hasPermission(BUDGET_PERMS.delete),
  submit: userStore.hasPermission(BUDGET_PERMS.submit),
  adjust: userStore.hasPermission(BUDGET_PERMS.adjust),
}));

// ==================== 表单 ====================

/** `create` 新建 / `edit` 改草稿 / `adjust` 调整（新版本 + 直接送审） */
type FormMode = 'create' | 'edit' | 'adjust';

const formVisible = ref(false);
const formMode = ref<FormMode>('create');
const editingRow = ref<BudgetRow | null>(null);
const adjustBase = ref<BudgetDetail | null>(null);
const changeReason = ref('');
const submitting = ref(false);
const formErrors = ref<string[]>([]);

const form = reactive<BudgetFormState>(emptyBudgetForm());

const formTitle = computed(() => {
  switch (formMode.value) {
    case 'edit':
      return `编辑预算草稿 ${editingRow.value?.budget_no ?? ''}`;
    case 'adjust':
      return `调整预算 ${editingRow.value?.budget_no ?? ''}（生成新版本并送审）`;
    default:
      return '新建预算';
  }
});

const formConfirmText = computed(() => {
  switch (formMode.value) {
    case 'edit':
      return '保存草稿';
    case 'adjust':
      return '保存并送审';
    default:
      return '创建草稿';
  }
});

const monthCheck = computed(() =>
  checkMonthDraft({ fiscalYear: form.fiscal_year, totalAmount: form.total_amount, rows: form.rows }),
);
const monthSum = computed(() => sumMonthText(form.rows));
const monthDiffText = computed(() => {
  if (monthCheck.value.diffCents === 0) {
    return '';
  }
  return formatMoney((monthCheck.value.diffCents / 100).toFixed(2));
});
const controlModeText = computed(() => controlModeLabel(form.control_mode === 'INHERIT' ? null : form.control_mode));

/** 表单字段级错误（把「请选择部门」这类文案挂到对应输入框下） */
function fieldError(field: 'dept_id' | 'account_id' | 'change_reason'): string {
  if (!formErrors.value.length) {
    return '';
  }
  const hit = formErrors.value.find((item) => {
    if (field === 'dept_id') {
      return item.includes('部门');
    }
    if (field === 'account_id') {
      return item.includes('科目');
    }
    return item.includes('调整原因');
  });
  return hit ?? '';
}

function resetFormErrors(): void {
  formErrors.value = [];
  changeReason.value = '';
}

function openCreate(): void {
  formMode.value = 'create';
  editingRow.value = null;
  adjustBase.value = null;
  resetFormErrors();
  Object.assign(form, emptyBudgetForm(filter.fiscal_year ?? new Date().getFullYear()));
  formVisible.value = true;
}

async function openEdit(row: BudgetRow): Promise<void> {
  formMode.value = 'edit';
  editingRow.value = row;
  adjustBase.value = null;
  resetFormErrors();
  const detail = await loadDetailForForm(row.id);
  if (!detail) {
    return;
  }
  Object.assign(form, {
    fiscal_year: detail.budget.fiscal_year,
    dept_id: detail.budget.dept_id,
    account_id: detail.budget.account_id,
    total_amount: centsToText(toCents(detail.budget.total_amount)),
    control_mode: detail.budget.control_mode ?? 'INHERIT',
    remark: detail.budget.remark ?? '',
    rows: monthRowsFromPeriods(detail.budget.fiscal_year, detail.periods),
  });
  formVisible.value = true;
}

async function openAdjust(row: BudgetRow): Promise<void> {
  formMode.value = 'adjust';
  editingRow.value = row;
  resetFormErrors();
  const detail = await loadDetailForForm(row.id);
  if (!detail) {
    return;
  }
  adjustBase.value = detail;
  Object.assign(form, {
    fiscal_year: detail.budget.fiscal_year,
    dept_id: detail.budget.dept_id,
    account_id: detail.budget.account_id,
    total_amount: centsToText(toCents(detail.budget.total_amount)),
    control_mode: detail.budget.control_mode ?? 'INHERIT',
    remark: detail.budget.remark ?? '',
    rows: monthRowsFromPeriods(detail.budget.fiscal_year, detail.periods),
  });
  formVisible.value = true;
}

/** 打开编辑 / 调整前先取详情（12 月分解只有详情里有） */
async function loadDetailForForm(id: string): Promise<BudgetDetail | null> {
  try {
    return await fetchBudgetDetail(id);
  } catch (error) {
    MessagePlugin.error(`读取预算详情失败：${errorText(error)}`);
    return null;
  }
}

function closeForm(): void {
  if (submitting.value) {
    return;
  }
  visibleReset();
}

function visibleReset(): void {
  formVisible.value = false;
  editingRow.value = null;
  adjustBase.value = null;
  formErrors.value = [];
  changeReason.value = '';
}

/** 切年度：12 行的 `period_code` 整组重算 */
function handleYearChange(value: unknown): void {
  form.rows = reyearRows(Number(value), form.rows);
}

/** 改额度后如果 12 行还是旧的，静默按平均分摊重算（用户可再手改） */
function applyEvenSplitSilently(): void {
  if (!form.total_amount.trim()) {
    return;
  }
  const filled = form.rows.some((row) => row.amount.trim() !== '');
  if (filled) {
    return;
  }
  form.rows = evenSplitRows(form.fiscal_year, form.total_amount);
}

/** 月度输入失焦：把用户输入的元归一成两位（`100` → `100.00`），避免 Δ 尾差来自格式 */
function touchMonth(index: number): void {
  const row = form.rows[index];
  if (!row) {
    return;
  }
  const text = row.amount.trim();
  if (!text) {
    row.amount = '';
    return;
  }
  row.amount = centsToText(toCents(text));
}

function applyEvenSplit(): void {
  if (!form.total_amount.trim()) {
    MessagePlugin.warning('请先填年度额度，再点平均分摊');
    return;
  }
  form.rows = evenSplitRows(form.fiscal_year, form.total_amount);
}

function applyTail(): void {
  const next = applyTailToDecember(form.rows, form.total_amount);
  if (!next) {
    MessagePlugin.warning('差额为负（前 11 个月已超过年度额度），尾差记 12 月会得到负数，请先调小前面的月份');
    return;
  }
  form.rows = next;
}

function applyPrior(): void {
  const prior = adjustBase.value?.periods ?? [];
  if (!prior.length) {
    MessagePlugin.info('新建时拿不到上年同槽基线，已按「平均分摊」处理；调整时才有上年月度占比可用');
    applyEvenSplit();
    return;
  }
  if (!form.total_amount.trim()) {
    MessagePlugin.warning('请先填年度额度，再点按上年占比');
    return;
  }
  const result = priorRatioRows(
    form.fiscal_year,
    form.total_amount,
    prior.map((item) => item.amount),
  );
  form.rows = result.rows;
  MessagePlugin.info(result.note);
}

async function submitForm(): Promise<void> {
  if (submitting.value) {
    return;
  }
  const errors = checkBudgetForm(form, formMode.value === 'edit' ? 'update' : formMode.value, changeReason.value);
  formErrors.value = errors;
  if (errors.length) {
    MessagePlugin.warning('请先修正表单里的红字项');
    return;
  }
  submitting.value = true;
  try {
    if (formMode.value === 'create') {
      const payload: CreateBudgetReq = toCreatePayload(form);
      const detail = await createBudget(payload);
      MessagePlugin.success(`已创建预算 ${detail.budget.budget_no}（草稿）`);
    } else if (formMode.value === 'edit' && editingRow.value) {
      const detail = await updateBudget(editingRow.value.id, toUpdatePayload(form));
      MessagePlugin.success(`已保存 ${detail.budget.budget_no}`);
    } else if (formMode.value === 'adjust' && editingRow.value) {
      const detail = await adjustBudget(editingRow.value.id, toAdjustPayload(form, changeReason.value));
      MessagePlugin.success(`已生成新版本 v${detail.budget.version_no} 并送审：${detail.budget.budget_no}`);
    }
    visibleReset();
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    submitting.value = false;
  }
}

// ==================== 行内动作 ====================

function handleAction(key: BudgetActionKey, row: BudgetRow): void {
  switch (key) {
    case 'detail':
      openDetail(row);
      break;
    case 'edit':
      void openEdit(row);
      break;
    case 'submit':
      confirmSubmit(row);
      break;
    case 'adjust':
      void openAdjust(row);
      break;
    case 'delete':
      confirmDelete(row);
      break;
  }
}

function confirmSubmit(row: BudgetRow): void {
  const dialog = DialogPlugin.confirm({
    header: '提交预算审批',
    body: `确认提交「${row.budget_no}」（${row.dept_name} / ${row.account_name} / ${formatMoney(row.total_amount)}）？提交后进入审批流，审批通过（ACTIVE）才能用于占用控制。`,
    confirmBtn: '提交审批',
    onConfirm: async () => {
      try {
        const detail = await submitBudget(row.id);
        MessagePlugin.success(`已提交：${detail.budget.budget_no}（${detail.budget.status_label}）`);
        dialog.hide();
        await loadRows();
      } catch (error) {
        MessagePlugin.error(errorText(error));
      }
    },
  });
}

function confirmDelete(row: BudgetRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除预算草稿',
    body: `确认删除「${row.budget_no}」？软删后不再出现在清单，已产生的版本链与占用记录保留。`,
    theme: 'danger',
    confirmBtn: '删除',
    onConfirm: async () => {
      try {
        await deleteBudget(row.id);
        MessagePlugin.success('已删除');
        dialog.hide();
        await loadRows();
      } catch (error) {
        MessagePlugin.error(errorText(error));
      }
    },
  });
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<BudgetDetail | null>(null);
const detailId = ref<string>('');

async function openDetail(row: BudgetRow): Promise<void> {
  detailId.value = row.id;
  detail.value = null;
  detailError.value = '';
  detailVisible.value = true;
  await refreshDetail();
}

async function refreshDetail(): Promise<void> {
  if (!detailId.value) {
    return;
  }
  detailLoading.value = true;
  detailError.value = '';
  try {
    detail.value = await fetchBudgetDetail(detailId.value);
  } catch (error) {
    detailError.value = errorText(error);
  } finally {
    detailLoading.value = false;
  }
}

// ==================== 数据加载 ====================

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const page = await fetchBudgetPage(budgetFilterParams(filter, { page: current.value, page_size: PAGE_SIZE }));
    rows.value = page.list ?? [];
    total.value = Number(page.total ?? 0);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `预算列表加载失败：${errorText(error)}`;
  } finally {
    loading.value = false;
  }
}

async function loadDepts(): Promise<void> {
  try {
    depts.value = await getDeptTree();
  } catch (error) {
    MessagePlugin.warning(`部门树加载失败：${errorText(error)}`);
  }
}

async function loadAccounts(): Promise<void> {
  try {
    accounts.value = await fetchBudgetAccountOptions(OPTION_LIMIT * 2);
  } catch (error) {
    MessagePlugin.warning(`预算科目加载失败（只显示末级启用科目）：${errorText(error)}`);
  }
}

function handleSearch(): void {
  current.value = 1;
  void loadRows();
}

function handleReset(): void {
  Object.assign(filter, emptyBudgetFilter());
  current.value = 1;
  void loadRows();
}

function refresh(): void {
  void loadRows();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadRows();
}

// ==================== 小工具 ====================

/**
 * ISO 时间 → `YYYY-MM-DD HH:mm`（北京时间，与后端 `today_cst()` 同口径）。
 * 后端出参是 `2026-11-04T18:33:12.835769Z`，直接摆到详情里太长且不是用户所在时区。
 */
function dateTimeText(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  const shifted = new Date(date.getTime() + 8 * 60 * 60 * 1000);
  const iso = shifted.toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)}`;
}

/** 列表「策略」列：继承时显示「继承」而不是空白 */
function controlModeShort(value: string | null | undefined): string {
  if (!value) {
    return '继承';
  }
  return value;
}

/**
 * 错误文案：后端 `ApiError` 出参是 `{ code, message }`，
 * 这里只拼一层「消息（码）」，具体怎么处理交给用户（不吞原始信息）。
 */
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
  void loadRows();
});
</script>

<style scoped lang="less">
.budget-page {
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

  &__year {
    width: 112px;
  }

  &__dept {
    width: 150px;
  }

  &__status {
    width: 108px;
  }

  &__change {
    width: 112px;
  }

  &__keyword {
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

.version-cell {
  display: flex;
  align-items: center;
  gap: 6px;
}

.op-cell {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
  font-size: 13px;
}

.money {
  font-variant-numeric: tabular-nums;

  &--strong {
    font-weight: 600;
  }

  &--bad {
    color: var(--td-error-color);
    font-variant-numeric: tabular-nums;
  }
}

.muted {
  color: var(--td-text-color-placeholder);

  &--hint {
    font-size: 12px;
    cursor: help;
  }
}

.sum-bad {
  color: var(--td-error-color);
}

.form-layout {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 240px;
  gap: 16px;

  &__main {
    min-width: 0;
  }
}

.form-side {
  align-self: start;
  border: 1px solid var(--td-component-stroke);
  border-radius: 8px;
  padding: 12px 14px;
  background: var(--td-bg-color-container);

  &__title {
    font-weight: 600;
    margin-bottom: 8px;
  }

  &__list {
    margin: 0;
    padding-left: 16px;
    font-size: 12px;
    line-height: 1.9;

    .is-bad {
      color: var(--td-error-color);
    }
  }

  &__note {
    margin-top: 10px;
    padding-top: 10px;
    border-top: 1px dashed var(--td-component-stroke);
    color: var(--td-text-color-placeholder);
    font-size: 12px;
    line-height: 1.7;
  }
}

.month-block {
  margin-top: 4px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 8px;
  padding: 12px;

  &__head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 10px;
  }

  &__title {
    font-weight: 600;
  }

  &__tip {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

.month-grid {
  display: grid;
  grid-template-columns: repeat(6, minmax(0, 1fr));
  gap: 8px;

  &__cell {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }
}

.month-foot {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-top: 10px;
  padding-top: 10px;
  border-top: 1px dashed var(--td-component-stroke);
  font-size: 13px;

  &__sums {
    display: flex;
    gap: 16px;
    align-items: baseline;
  }

  &__actions {
    display: flex;
    gap: 8px;
  }
}

.form-alert {
  margin-top: 12px;
}

.drawer {
  display: flex;
  flex-direction: column;
  gap: 16px;

  &-loading {
    padding: 40px 0;
    text-align: center;
    color: var(--td-text-color-placeholder);
  }

  &-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;

    &__main {
      display: flex;
      align-items: center;
      gap: 8px;
    }

    &__title {
      font-size: 16px;
      font-weight: 600;
    }
  }

  &-usage {
    border: 1px solid var(--td-component-stroke);
    border-radius: 8px;
    padding: 10px 12px;

    &__label {
      display: flex;
      justify-content: space-between;
      font-size: 12px;
      color: var(--td-text-color-secondary);
      margin-bottom: 6px;
    }
  }

  &-section {
    display: flex;
    flex-direction: column;
    gap: 8px;

    &__title {
      font-weight: 600;
    }
  }

  &-empty {
    padding: 16px;
    border: 1px dashed var(--td-component-stroke);
    border-radius: 8px;
    color: var(--td-text-color-placeholder);
    font-size: 12px;
    text-align: center;
  }
}
</style>
