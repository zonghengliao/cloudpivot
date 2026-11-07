<!--
  我的报销页（FIN-3-FE / ARCH-6 §4.10）。

  这一页承担 §4.10 的四个交互细节里的前两个：
  ① 明细**行内编辑**：增删行、只录含税、右侧实时拆出「不含税 / 税额」（口径同后端
     `expense_compute::split`，见 `expense-logic.ts::splitPreview`）；
  ② 费用类型不可抵扣（招待费 / 福利费）时，**税额输入框置灰显示 0** 并提示
     「招待费进项税不得抵扣」。

  另外两条落在详情抽屉里（`ExpenseDetailDrawer.vue`）：预算占用区块、审批轨迹。

  # 默认只查自己

  「我的报销」默认带 `reporter_id = 当前登录用户 id`（§4.10 细节 1）。开关在工具栏上，
  关掉才看别人的单 —— 后端只提供 `reporter_id` 筛选，不替前端决定「谁该看谁的单」。

  # 为什么金额比较全走整数分

  `toCents` / `formatCents`（`shared/money.ts`）：后端 numeric 出 JSON 是字符串，
  浮点累加会在月末对不上账。页面上的合计只是预览，**入账金额永远以接口返回为准**。
-->
<template>
  <div class="expense-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="expense-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建报销
        </t-button>
        <t-button variant="outline" data-testid="expense-btn-refresh" @click="refresh">刷新</t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="expense-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.pay_status"
          class="tool-row__status"
          :options="payStatusFilterOptions"
          placeholder="全部付款状态"
          clearable
          data-testid="expense-filter-pay-status"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="单号 / 事由（模糊）"
          clearable
          data-testid="expense-filter-keyword"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="expense-btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="expense-btn-reset" @click="handleReset">重置</t-button>
      </div>

      <div class="scope-row">
        <span class="scope-row__text">
          当前口径：{{ mineOnly ? '只看我的报销单' : '全部报销单（按数据范围）' }}
          <span v-if="mineOnly && !myId" class="scope-row__warn">（还没拿到当前用户 id，本轮未带 reporter_id）</span>
        </span>
        <t-switch
          v-model="mineOnly"
          size="small"
          data-testid="expense-switch-mine"
          @change="handleSearch"
        >
          <template #label>只看我的</template>
        </t-switch>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">报销单</span>
          <span class="panel-head__meta" data-testid="expense-meta">共 {{ total }} 张</span>
        </div>
        <span class="panel-head__tip">
          提交后按金额分级走审批；审批通过自动生成计提凭证，付款登记在「待付款」页
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="expense-load-error"
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
        data-testid="expense-table"
        @page-change="handlePageChange"
      >
        <template #report_no="{ row }">
          <span class="mono" :data-testid="`expense-no-${row.report_no}`">{{ row.report_no }}</span>
        </template>
        <template #title="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.title }}</span>
            <span class="name-cell__sub">{{ row.reporter_name }} · {{ row.dept_name || '—' }} · {{ row.item_count }} 条明细</span>
          </div>
        </template>
        <template #total_with_tax="{ row }">
          <span class="money money--strong">{{ formatMoney(row.total_with_tax) }}</span>
        </template>
        <template #budget="{ row }">
          <t-tag
            v-if="budgetFlag(row)"
            size="small"
            variant="light"
            :theme="budgetCell(row).theme"
            :data-testid="`expense-budget-${row.report_no}`"
          >
            {{ budgetCell(row).text }}
          </t-tag>
          <span v-else class="muted">—</span>
        </template>
        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="statusTheme(row.status)"
            :data-testid="`expense-status-${row.report_no}`"
          >
            {{ statusText(row) }}
          </t-tag>
        </template>
        <template #pay_status="{ row }">
          <t-tag size="small" variant="outline" :theme="payStatusTheme(row.pay_status)">
            {{ payStatusText(row) }}
          </t-tag>
        </template>
        <template #op="{ row }">
          <div class="op-cell">
            <t-button variant="text" size="small" data-testid="expense-op-detail" @click="openDetail(row)">
              详情
            </t-button>
            <t-button
              v-if="canEditReport(row)"
              v-privilege="[PERM.update]"
              variant="text"
              size="small"
              data-testid="expense-op-edit"
              @click="openEdit(row)"
            >
              编辑
            </t-button>
            <t-button
              v-if="canSubmitReport(row)"
              v-privilege="[PERM.submit]"
              variant="text"
              size="small"
              data-testid="expense-op-submit"
              @click="handleSubmit(row)"
            >
              提交
            </t-button>
            <t-button
              v-if="canWithdrawReport(row)"
              v-privilege="[PERM.submit]"
              variant="text"
              size="small"
              data-testid="expense-op-withdraw"
              @click="handleWithdraw(row)"
            >
              撤回
            </t-button>
            <t-button
              v-if="canCancelReport(row)"
              v-privilege="[PERM.cancel]"
              variant="text"
              size="small"
              theme="danger"
              data-testid="expense-op-cancel"
              @click="openCancel(row)"
            >
              作废
            </t-button>
            <t-button
              v-if="canDeleteReport(row)"
              v-privilege="[PERM.remove]"
              variant="text"
              size="small"
              theme="danger"
              data-testid="expense-op-delete"
              @click="handleDelete(row)"
            >
              删除
            </t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 新建 / 编辑草稿 ==================== -->
    <t-dialog
      v-model:visible="dialogVisible"
      :header="editingId ? `编辑草稿 ${editingNo}` : '新建报销单'"
      width="1240px"
      :close-on-overlay-click="false"
      :footer="false"
      data-testid="expense-form-dialog"
    >
      <t-form label-width="88px" :data="form" class="form">
        <t-row :gutter="[16, 8]">
          <t-col :span="12">
            <t-form-item label="事由" :help="formErrors.title" :status="fieldStatus('title')">
              <t-input
                v-model="form.title"
                :maxlength="MAX_TITLE_LEN"
                placeholder="如：2026-11 上海客户拜访差旅费"
                data-testid="expense-form-title"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="费用发生日">
              <t-date-picker
                v-model="form.expense_date"
                format="YYYY-MM-DD"
                clearable
                data-testid="expense-form-date"
              />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="报销人">
              <t-input :value="reporterLabel" disabled data-testid="expense-form-reporter" />
            </t-form-item>
          </t-col>
          <t-col :span="24">
            <t-form-item label="备注">
              <t-textarea v-model="form.remark" :maxlength="2000" placeholder="可选" />
            </t-form-item>
          </t-col>
        </t-row>

        <!-- ---------------- 明细（行内编辑） ---------------- -->
        <div class="items" data-testid="expense-items">
          <div class="items__head">
            <span class="items__title">明细</span>
            <span class="items__hint">
              只录<strong>含税</strong>；不含税与税额由服务端按「类型是否可抵扣」拆（预览同口径）
            </span>
          </div>

          <div class="items__scroll">
            <div class="items__grid items__grid--head">
              <span>费用类型</span>
              <span>含税金额</span>
              <span>税率</span>
              <span class="ta-right">不含税</span>
              <span class="ta-right">税额</span>
              <span>摘要</span>
              <span>发票号</span>
              <span>发票类型</span>
              <span>开票日期</span>
              <span />
            </div>

            <div
              v-for="(item, index) in form.items"
              :key="item.key"
              class="items__row"
              :data-testid="`expense-item-${index}`"
            >
              <div class="items__grid">
                <div class="items__cell">
                  <t-select
                    v-model="item.expense_type_id"
                    :options="typeOptions"
                    placeholder="选择费用类型"
                    filterable
                    :data-testid="`expense-item-type-${index}`"
                    @change="handleTypeChange(item)"
                  />
                  <p v-if="rowErrors(index).expense_type_id" class="cell-error">
                    {{ rowErrors(index).expense_type_id }}
                  </p>
                </div>

                <div class="items__cell">
                  <t-input
                    v-model="item.with_tax_amount"
                    placeholder="含税金额"
                    :data-testid="`expense-item-amount-${index}`"
                  />
                  <p v-if="rowErrors(index).with_tax_amount" class="cell-error">
                    {{ rowErrors(index).with_tax_amount }}
                  </p>
                </div>

                <div class="items__cell">
                  <t-select
                    v-model="item.tax_rate"
                    :options="taxRateOptions"
                    :disabled="!deductibleOf(item)"
                    :data-testid="`expense-item-rate-${index}`"
                  />
                  <p v-if="rowErrors(index).tax_rate" class="cell-error">{{ rowErrors(index).tax_rate }}</p>
                </div>

                <div class="items__cell items__cell--num">
                  <span class="money" :data-testid="`expense-item-net-${index}`">
                    {{ centsText(previewOf(item).amountCents) }}
                  </span>
                </div>

                <!-- ★ 交互细节②：不可抵扣 → 置灰显示 0 + 提示 -->
                <div class="items__cell">
                  <t-input
                    :value="centsText(previewOf(item).taxCents)"
                    disabled
                    :class="{ 'tax-input--locked': !deductibleOf(item) }"
                    :data-testid="`expense-item-tax-${index}`"
                  />
                  <p v-if="!deductibleOf(item)" class="cell-hint" data-testid="expense-nondeductible-hint">
                    {{ NON_DEDUCTIBLE_HINT }}
                  </p>
                </div>

                <div class="items__cell">
                  <t-input v-model="item.summary" :maxlength="MAX_SUMMARY_LEN" placeholder="行摘要" />
                  <p v-if="rowErrors(index).summary" class="cell-error">{{ rowErrors(index).summary }}</p>
                </div>

                <div class="items__cell">
                  <t-input v-model="item.invoice_no" placeholder="可选" />
                  <p v-if="rowErrors(index).invoice_no" class="cell-error">{{ rowErrors(index).invoice_no }}</p>
                </div>

                <div class="items__cell">
                  <t-select
                    v-model="item.invoice_type"
                    :options="invoiceTypeOptions"
                    placeholder="不填"
                    clearable
                  />
                </div>

                <div class="items__cell">
                  <t-date-picker v-model="item.invoice_date" format="YYYY-MM-DD" clearable />
                </div>

                <div class="items__cell items__cell--op">
                  <t-tooltip content="删除该行">
                    <t-button
                      variant="text"
                      shape="square"
                      :disabled="form.items.length <= 1"
                      :data-testid="`expense-item-remove-${index}`"
                      @click="removeItem(index)"
                    >
                      <template #icon><t-icon name="delete" /></template>
                    </t-button>
                  </t-tooltip>
                </div>
              </div>
            </div>
          </div>

          <t-button variant="dashed" block data-testid="expense-btn-add-item" @click="addItem">
            <template #icon><t-icon name="add" /></template>
            添加明细
          </t-button>
          <p v-if="formErrors.items" class="cell-error">{{ formErrors.items }}</p>
        </div>

        <!-- ---------------- 合计 ---------------- -->
        <div class="totals" data-testid="expense-totals">
          <span class="totals__item">
            不含税合计
            <strong data-testid="expense-total-net">{{ centsText(totals.amountCents) }}</strong>
          </span>
          <span class="totals__item">
            税额
            <strong data-testid="expense-total-tax">{{ centsText(totals.taxCents) }}</strong>
          </span>
          <span class="totals__item totals__item--strong">
            价税合计
            <strong data-testid="expense-total-with-tax">{{ centsText(totals.withTaxCents) }}</strong>
          </span>
          <span class="totals__item totals__item--tip">共 {{ totals.itemCount }} 行</span>
        </div>
      </t-form>

      <div class="dialog-footer">
        <t-button variant="outline" data-testid="expense-form-cancel" @click="dialogVisible = false">取消</t-button>
        <t-button theme="primary" variant="outline" :loading="saving" data-testid="expense-form-save" @click="handleSave(false)">
          保存草稿
        </t-button>
        <t-button
          v-privilege="[PERM.submit]"
          theme="primary"
          :loading="saving"
          data-testid="expense-form-save-submit"
          @click="handleSave(true)"
        >
          保存并提交审批
        </t-button>
      </div>
    </t-dialog>

    <!-- ==================== ④ 作废（原因 ≥10 字） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      header="作废报销单"
      width="560px"
      :confirm-btn="{ content: '确认作废', theme: 'danger', loading: cancelling }"
      data-testid="expense-cancel-dialog"
      @confirm="handleCancel"
    >
      <p class="cancel-tip">
        作废「{{ cancelTarget?.report_no }}」后不可恢复；若已入账，后端会自动生成红冲凭证并释放预算占用。
      </p>
      <t-textarea
        v-model="cancelReason"
        :maxlength="500"
        placeholder="请填写作废原因（至少 10 个字，会写进状态日志）"
        data-testid="expense-cancel-reason"
      />
      <p class="cell-error" v-if="cancelError" data-testid="expense-cancel-error">{{ cancelError }}</p>
      <p class="cancel-count">{{ [...cancelReason].length }} / {{ MIN_CANCEL_REASON_LEN }} 字起</p>
    </t-dialog>

    <!-- ==================== ⑤ 详情抽屉 ==================== -->
    <ExpenseDetailDrawer v-model="detailVisible" :report-id="detailId" />
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  cancelExpense,
  createExpense,
  deleteExpense,
  fetchExpenseDetail,
  fetchExpensePage,
  fetchExpenseTypeList,
  submitExpense,
  updateExpense,
  withdrawExpense,
  EXPENSE_INVOICE_TYPES,
  EXPENSE_INVOICE_TYPE_LABELS,
  type ExpenseReport,
  type ExpenseReportQuery,
  type ExpenseType,
} from '@/api/business/erp/expense';
import { useUserStore } from '@/stores/user';
import { formatMoney } from '@/views/business/erp/shared/money';
import {
  budgetFlag,
  canCancelReport,
  canDeleteReport,
  canEditReport,
  canSubmitReport,
  canWithdrawReport,
  cancelReasonError,
  centsText,
  emptyExpenseForm,
  emptyItemForm,
  firstError,
  formFromDetail,
  MAX_SUMMARY_LEN,
  MAX_TITLE_LEN,
  MIN_CANCEL_REASON_LEN,
  NON_DEDUCTIBLE_HINT,
  payStatusText,
  payStatusTheme,
  previewItem,
  previewTotals,
  STATUS_FILTER_OPTIONS,
  statusText,
  statusTheme,
  TAX_RATE_OPTIONS,
  toReportPayload,
  typeDeductible,
  validateExpenseForm,
  type ExpenseForm,
  type ExpenseFormErrors,
  type ExpenseItemForm,
} from './expense-logic';
import ExpenseDetailDrawer from './ExpenseDetailDrawer.vue';

/** 本页权限码（V0242 种的 12 个码里的 8 个；一个都不多加） */
const PERM = {
  list: 'business:erp:expense:list',
  create: 'business:erp:expense:create',
  update: 'business:erp:expense:update',
  remove: 'business:erp:expense:delete',
  submit: 'business:erp:expense:submit',
  cancel: 'business:erp:expense:cancel',
  detail: 'business:erp:expense:detail',
} as const;

/** 每页条数 */
const PAGE_SIZE = 20;

const userStore = useUserStore();
/** 当前用户 id（「只看我的」的 reporter_id） */
const myId = computed(() => userStore.userInfo?.id ?? '');
const reporterLabel = computed(
  () => userStore.userInfo?.real_name || userStore.userInfo?.username || '当前登录用户',
);

// ==================== 列表 ====================

const rows = ref<ExpenseReport[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');
const mineOnly = ref(true);

const filter = reactive({
  keyword: '',
  status: '',
  pay_status: '',
});

const statusFilterOptions = STATUS_FILTER_OPTIONS;
const payStatusFilterOptions = [
  { label: '未付款', value: 'UNPAID' },
  { label: '部分付款', value: 'PARTIAL' },
  { label: '已付清', value: 'PAID' },
];

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// 列宽合计 ≈ 942px：1280 视口（侧栏展开时表格可视宽约 968px）不会让固定列压住状态列
const columns: PrimaryTableCol[] = [
  { colKey: 'report_no', title: '单号', width: 150 },
  { colKey: 'title', title: '事由 / 报销人 · 部门', width: 200, ellipsis: true },
  { colKey: 'expense_date', title: '发生日', width: 92 },
  { colKey: 'total_with_tax', title: '价税合计', width: 100, align: 'right' },
  // FIX-5：真超支（红「超预算」）与没编预算（黄「未编预算」）必须一眼分得开
  { colKey: 'budget', title: '预算', width: 92 },
  { colKey: 'status', title: '状态', width: 104 },
  { colKey: 'pay_status', title: '付款', width: 76 },
  { colKey: 'op', title: '操作', width: 220, fixed: 'right' },
];

/** 列表格里的预算标记（`null` → 灰色 `—`，模板里不用写非空断言）。 */
function budgetCell(row: ExpenseReport): { text: string; theme: 'default' | 'danger' | 'warning' } {
  return budgetFlag(row) ?? { text: '—', theme: 'default' };
}

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const query: ExpenseReportQuery = {
      page: current.value,
      page_size: PAGE_SIZE,
      keyword: filter.keyword.trim(),
      status: filter.status,
      pay_status: filter.pay_status,
    };
    // §4.10 细节 1：默认只查自己；空串会被后端当「等于空串」，所以只在拿到 id 时才带
    if (mineOnly.value && myId.value) {
      query.reporter_id = myId.value;
    }
    const page = await fetchExpensePage(query);
    rows.value = page.list ?? [];
    total.value = Number(page.total ?? 0);
  } catch (error) {
    loadError.value = errorText(error);
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
  filter.keyword = '';
  filter.status = '';
  filter.pay_status = '';
  mineOnly.value = true;
  handleSearch();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadRows();
}

function refresh(): void {
  void loadRows();
}

// ==================== 表单 ====================

const dialogVisible = ref(false);
const editingId = ref<string | null>(null);
const editingNo = ref('');
const form = reactive<ExpenseForm>(emptyExpenseForm());
const formErrors = ref<ExpenseFormErrors>({});
const saving = ref(false);
const expenseTypes = ref<ExpenseType[]>([]);

const typeOptions = computed(() =>
  expenseTypes.value.map((item) => ({
    label: `${item.type_name}（${item.type_code}${item.tax_deductible ? '' : ' · 不可抵扣'}）`,
    value: item.id,
  })),
);

const taxRateOptions = TAX_RATE_OPTIONS;
const invoiceTypeOptions = EXPENSE_INVOICE_TYPES.map((value) => ({
  label: EXPENSE_INVOICE_TYPE_LABELS[value],
  value,
}));

const totals = computed(() => previewTotals(form, expenseTypes.value));

/** 明细预览（整数分） */
function previewOf(item: ExpenseItemForm) {
  return previewItem(item, expenseTypes.value);
}

/** 类型是否可抵扣（找不到类型按可抵扣处理，最终以后端裁决为准） */
function deductibleOf(item: ExpenseItemForm): boolean {
  return typeDeductible(item.expense_type_id, expenseTypes.value);
}

function fieldStatus(field: string): 'error' | undefined {
  return formErrors.value[field] ? 'error' : undefined;
}

function rowErrors(index: number): ExpenseFormErrors {
  const out: ExpenseFormErrors = {};
  const prefix = `items.${index}.`;
  for (const [key, value] of Object.entries(formErrors.value)) {
    if (key.startsWith(prefix)) {
      out[key.slice(prefix.length)] = value;
    }
  }
  return out;
}

function resetForm(): void {
  Object.assign(form, emptyExpenseForm());
  formErrors.value = {};
}

function openCreate(): void {
  resetForm();
  editingId.value = null;
  editingNo.value = '';
  dialogVisible.value = true;
}

async function openEdit(row: ExpenseReport): Promise<void> {
  try {
    const detail = await fetchExpenseDetail(row.id);
    Object.assign(form, formFromDetail(detail));
    formErrors.value = {};
    editingId.value = row.id;
    editingNo.value = row.report_no;
    dialogVisible.value = true;
  } catch (error) {
    MessagePlugin.error(errorText(error));
  }
}

function addItem(): void {
  form.items.push(emptyItemForm());
}

function removeItem(index: number): void {
  if (form.items.length <= 1) {
    MessagePlugin.warning('至少保留一条明细');
    return;
  }
  form.items.splice(index, 1);
  formErrors.value = {};
}

/** 切换类型：不可抵扣 → 税率归 0（税额显示 0）；必填发票号的行给出提示 */
function handleTypeChange(item: ExpenseItemForm): void {
  if (!deductibleOf(item)) {
    item.tax_rate = '0';
  }
}

async function handleSave(submitAfter: boolean): Promise<void> {
  const errors = validateExpenseForm(form, expenseTypes.value);
  formErrors.value = errors;
  const first = firstError(errors);
  if (first) {
    MessagePlugin.warning(first);
    return;
  }
  saving.value = true;
  try {
    const payload = toReportPayload(form);
    const saved = editingId.value
      ? await updateExpense(editingId.value, payload)
      : await createExpense(payload);
    if (submitAfter) {
      await submitExpense(saved.report.id);
      MessagePlugin.success('已保存并提交审批');
    } else {
      MessagePlugin.success('已保存草稿');
    }
    dialogVisible.value = false;
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    saving.value = false;
  }
}

// ==================== 动作 ====================

function confirmAction(header: string, body: string, run: () => Promise<void>): void {
  const dialog = DialogPlugin.confirm({
    header,
    body,
    theme: 'warning',
    onConfirm: async () => {
      try {
        await run();
        dialog.hide();
      } catch (error) {
        MessagePlugin.error(errorText(error));
      }
    },
  });
}

function handleSubmit(row: ExpenseReport): void {
  confirmAction(
    '提交审批',
    `确认提交「${row.report_no}」？提交后按金额分级进入审批流，审批通过会自动生成计提凭证并占用预算。`,
    async () => {
      await submitExpense(row.id);
      MessagePlugin.success('已提交审批');
      await loadRows();
    },
  );
}

function handleWithdraw(row: ExpenseReport): void {
  confirmAction(
    '撤回报销单',
    `确认撤回「${row.report_no}」？撤回后回到草稿，可继续编辑再提交。`,
    async () => {
      await withdrawExpense(row.id);
      MessagePlugin.success('已撤回为草稿');
      await loadRows();
    },
  );
}

function handleDelete(row: ExpenseReport): void {
  confirmAction(
    '删除草稿',
    `确认删除「${row.report_no}」？软删后不再出现在列表，历史不留痕（未提交的单据可直接删）。`,
    async () => {
      await deleteExpense(row.id);
      MessagePlugin.success('已删除');
      await loadRows();
    },
  );
}

// ==================== 作废 ====================

const cancelVisible = ref(false);
const cancelTarget = ref<ExpenseReport | null>(null);
const cancelReason = ref('');
const cancelling = ref(false);
const cancelError = computed(() => (cancelReason.value ? cancelReasonError(cancelReason.value) : ''));

function openCancel(row: ExpenseReport): void {
  cancelTarget.value = row;
  cancelReason.value = '';
  cancelVisible.value = true;
}

async function handleCancel(): Promise<void> {
  const target = cancelTarget.value;
  if (!target) {
    return;
  }
  const error = cancelReasonError(cancelReason.value);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  cancelling.value = true;
  try {
    await cancelExpense(target.id, cancelReason.value.trim());
    MessagePlugin.success('已作废（入账的会红冲，预算占用已释放）');
    cancelVisible.value = false;
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    cancelling.value = false;
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailId = ref<string | null>(null);

function openDetail(row: ExpenseReport): void {
  detailId.value = row.id;
  detailVisible.value = true;
}

// ==================== 初始化 ====================

async function loadTypes(): Promise<void> {
  try {
    expenseTypes.value = await fetchExpenseTypeList();
  } catch (error) {
    MessagePlugin.warning(`费用类型加载失败：${errorText(error)}`);
  }
}

function errorText(error: unknown): string {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
}

onMounted(async () => {
  await Promise.all([loadTypes(), loadRows()]);
});
</script>

<style scoped>
.expense-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.tool-row__spacer {
  flex: 1;
}

.tool-row__status {
  width: 150px;
}

.tool-row__keyword {
  width: 220px;
}

.scope-row {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.scope-row__warn {
  color: var(--td-warning-color);
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
}

.panel-head__title {
  font-size: 15px;
  font-weight: 600;
}

.panel-head__meta {
  margin-left: 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.panel-head__tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.list-alert {
  margin-bottom: 12px;
}

.name-cell {
  display: flex;
  flex-direction: column;
  line-height: 1.35;
}

.name-cell__main {
  color: var(--td-text-color-primary);
}

.name-cell__sub {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.op-cell {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 2px;
}

.money {
  font-variant-numeric: tabular-nums;
  font-family: var(--td-font-family-number, monospace);
}

.money--strong {
  font-weight: 600;
}

.mono {
  font-family: var(--td-font-family-number, monospace);
}

/* ---------------- 表单 ---------------- */

.items {
  margin-top: 8px;
}

/* 表单里的下拉 / 输入框一律撑满所在列（TDesign 的 t-select / t-input-number 默认按内容宽度，
   在 t-col 里会缩成一小截，同一行左右参差不齐） */
.form :deep(.t-select__wrap),
.form :deep(.t-input-number) {
  width: 100%;
}

.form :deep(.t-input) {
  width: 100%;
}

.items__head {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-bottom: 8px;
}

.items__title {
  font-size: 14px;
  font-weight: 600;
}

.items__hint {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.items__scroll {
  overflow-x: auto;
}

.items__grid {
  display: grid;
  grid-template-columns: 170px 110px 90px 90px 132px 160px 120px 110px 120px 40px;
  gap: 8px;
  align-items: start;
  min-width: 1142px;
}

.items__grid--head {
  padding: 6px 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  border-bottom: 1px solid var(--td-component-border);
}

.items__row {
  padding: 8px 0;
  border-bottom: 1px dashed var(--td-component-stroke);
}

.items__cell--num {
  padding-top: 7px;
  text-align: right;
}

.items__cell--op {
  padding-top: 2px;
}

.ta-right {
  text-align: right;
}

.tax-input--locked :deep(input) {
  background: var(--td-bg-color-container-hover);
  color: var(--td-text-color-placeholder);
}

.cell-error {
  margin: 2px 0 0;
  color: var(--td-error-color);
  font-size: 12px;
  line-height: 1.3;
}

.cell-hint {
  margin: 2px 0 0;
  color: var(--td-warning-color);
  font-size: 11px;
  line-height: 1.25;
  white-space: nowrap;
}

.totals {
  display: flex;
  align-items: baseline;
  justify-content: flex-end;
  gap: 20px;
  margin-top: 12px;
  padding-top: 10px;
  border-top: 1px solid var(--td-component-border);
}

.totals__item {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.totals__item strong {
  margin-left: 4px;
  color: var(--td-text-color-primary);
  font-family: var(--td-font-family-number, monospace);
  font-variant-numeric: tabular-nums;
}

.totals__item--strong strong {
  font-size: 16px;
}

.totals__item--tip {
  font-size: 12px;
}

.dialog-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 16px;
}

.cancel-tip {
  margin: 0 0 8px;
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.cancel-count {
  margin: 4px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  text-align: right;
}
</style>
