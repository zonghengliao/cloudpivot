<!--
  待付款页（FIN-3-FE / ARCH-6 §4.10）。

  口径只有一条：**后端已经把「能付的」筛好了** —— `GET /business/erp/expense/pay/page`
  的 SQL 固定是 `status = 'APPROVED' AND pay_status <> 'PAID'`，且只认 `page / page_size`
  两个参数。所以这一页**不摆筛选框**（摆了也不生效＝骗用户），只做列表 + 付款登记。

  付款登记的三条硬约束（后端 409/400，前端先拦）：
  1. 金额 > 0 且 ≤ 未付金额（部分付款后还能再付，付清即 PAID）；
  2. 付款日期必填（缺省今天）；
  3. 付款方式 ∈ 银行转账 / 现金 / 冲抵，账户科目缺省 1002。

  付款成功后后端会写付款行 + 付款凭证，并回写 `paid_amount / unpaid_amount / pay_status`，
  所以这里付完就重拉列表，**不在前端自己加**（自己加会和行锁下的真实结果打架）。
-->
<template>
  <div class="pay-page">
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="expense-pay-btn-refresh" @click="refresh">刷新</t-button>
        <span class="tool-row__spacer" />
        <span class="tool-row__tip">
          仅显示「已审批且未付清」的报销单（按审批通过时间升序，先批的先付）
        </span>
      </div>
    </t-card>

    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">待付款报销单</span>
          <span class="panel-head__meta" data-testid="expense-pay-meta">共 {{ total }} 张</span>
        </div>
        <span class="panel-head__tip">付款登记会写付款凭证；已付过款的单据不可作废（需先红冲付款）</span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="expense-pay-load-error"
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
        data-testid="expense-pay-table"
        @page-change="handlePageChange"
      >
        <template #report_no="{ row }">
          <span class="mono" :data-testid="`expense-pay-no-${row.report_no}`">{{ row.report_no }}</span>
        </template>
        <template #title="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.title }}</span>
            <span class="name-cell__sub">{{ row.reporter_name }} · {{ row.dept_name || '—' }}</span>
          </div>
        </template>
        <template #approved_at="{ row }">
          <span class="muted">{{ shortTime(row.approved_at) }}</span>
        </template>
        <template #total_with_tax="{ row }">
          <span class="money">{{ formatMoney(row.total_with_tax) }}</span>
        </template>
        <template #paid_amount="{ row }">
          <span class="money muted">{{ formatMoney(row.paid_amount) }}</span>
        </template>
        <template #unpaid_amount="{ row }">
          <span class="money money--strong money--warn" :data-testid="`expense-pay-unpaid-${row.report_no}`">
            {{ formatMoney(row.unpaid_amount) }}
          </span>
        </template>
        <template #pay_status="{ row }">
          <t-tag size="small" variant="light" :theme="payStatusTheme(row.pay_status)">
            {{ payStatusText(row) }}
          </t-tag>
        </template>
        <template #op="{ row }">
          <div class="op-cell">
            <t-button variant="text" size="small" data-testid="expense-pay-op-detail" @click="openDetail(row)">
              详情
            </t-button>
            <t-button
              v-if="canPayReport(row)"
              v-privilege="[PERM.pay]"
              variant="text"
              size="small"
              theme="primary"
              data-testid="expense-pay-op-pay"
              @click="openPay(row)"
            >
              付款登记
            </t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== 付款登记 ==================== -->
    <t-dialog
      v-model:visible="payVisible"
      header="付款登记"
      width="560px"
      :confirm-btn="{ content: '确认付款', theme: 'primary', loading: paying }"
      data-testid="expense-pay-dialog"
      @confirm="handlePay"
    >
      <div v-if="payTarget" class="pay-head">
        <div class="pay-head__row">
          <span class="pay-head__label">单号</span>
          <span class="mono">{{ payTarget.report_no }}</span>
        </div>
        <div class="pay-head__row">
          <span class="pay-head__label">事由</span>
          <span>{{ payTarget.title }}</span>
        </div>
        <div class="pay-head__row">
          <span class="pay-head__label">未付金额</span>
          <span class="money money--strong" data-testid="expense-pay-dialog-unpaid">
            {{ formatMoney(payTarget.unpaid_amount) }}
          </span>
        </div>
        <div class="pay-head__row">
          <span class="pay-head__label">已付 / 合计</span>
          <span class="money muted">
            {{ formatMoney(payTarget.paid_amount) }} / {{ formatMoney(payTarget.total_with_tax) }}
          </span>
        </div>
      </div>

      <t-form label-width="92px" :data="payForm" class="pay-form">
        <t-form-item label="本次付款" :status="payError ? 'error' : undefined" :help="payError || undefined">
          <t-input v-model="payForm.amount" placeholder="不超过未付金额" data-testid="expense-pay-amount" />
        </t-form-item>
        <t-form-item label="付款日期">
          <t-date-picker v-model="payForm.pay_date" format="YYYY-MM-DD" data-testid="expense-pay-date" />
        </t-form-item>
        <t-form-item label="付款方式">
          <t-select v-model="payForm.pay_method" :options="payMethodOptions" data-testid="expense-pay-method" />
        </t-form-item>
        <t-form-item label="付款账户">
          <t-input v-model="payForm.pay_account_code" placeholder="科目编码，缺省 1002" data-testid="expense-pay-account" />
        </t-form-item>
        <t-form-item label="备注">
          <t-input v-model="payForm.remark" :maxlength="200" placeholder="可选" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <ExpenseDetailDrawer v-model="detailVisible" :report-id="detailId" />
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  fetchExpensePayPage,
  payExpense,
  EXPENSE_PAY_METHODS,
  EXPENSE_PAY_METHOD_LABELS,
  type ExpenseReport,
} from '@/api/business/erp/expense';
import { formatMoney } from '@/views/business/erp/shared/money';
import {
  canPayReport,
  emptyPayForm,
  payStatusText,
  payStatusTheme,
  toPayPayload,
  validatePayForm,
  type ExpensePayForm,
} from '../expense-logic';
import ExpenseDetailDrawer from '../ExpenseDetailDrawer.vue';

/** 本页用到的权限码 */
const PERM = {
  list: 'business:erp:expense:list',
  pay: 'business:erp:expense:pay',
  detail: 'business:erp:expense:detail',
} as const;

const PAGE_SIZE = 20;

const rows = ref<ExpenseReport[]>([]);
const total = ref(0);
const current = ref(1);
const loading = ref(false);
const loadError = ref('');

const pagination = computed(() => ({
  current: current.value,
  pageSize: PAGE_SIZE,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

// 列宽合计 ≈ 940px：1280 视口（侧栏展开时表格可视宽约 968px）不会让固定列压住状态列
const columns: PrimaryTableCol[] = [
  { colKey: 'report_no', title: '单号', width: 150 },
  { colKey: 'title', title: '事由 / 报销人 · 部门', width: 180, ellipsis: true },
  { colKey: 'approved_at', title: '审批通过', width: 108 },
  { colKey: 'total_with_tax', title: '价税合计', width: 100, align: 'right' },
  { colKey: 'paid_amount', title: '已付', width: 90, align: 'right' },
  { colKey: 'unpaid_amount', title: '未付', width: 100, align: 'right' },
  { colKey: 'pay_status', title: '付款状态', width: 90 },
  { colKey: 'op', title: '操作', width: 120, fixed: 'right' },
];

async function loadRows(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const page = await fetchExpensePayPage(current.value, PAGE_SIZE);
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

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadRows();
}

function refresh(): void {
  void loadRows();
}

// ==================== 付款 ====================

const payVisible = ref(false);
const payTarget = ref<ExpenseReport | null>(null);
const payForm = reactive<ExpensePayForm>({
  amount: '',
  pay_date: '',
  pay_method: 'BANK_TRANSFER',
  pay_account_code: '1002',
  remark: '',
});
const paying = ref(false);
const payError = ref('');

const payMethodOptions = EXPENSE_PAY_METHODS.map((value) => ({
  label: EXPENSE_PAY_METHOD_LABELS[value],
  value,
}));

function openPay(row: ExpenseReport): void {
  payTarget.value = row;
  Object.assign(payForm, emptyPayForm(row));
  payError.value = '';
  payVisible.value = true;
}

async function handlePay(): Promise<void> {
  const target = payTarget.value;
  if (!target) {
    return;
  }
  const error = validatePayForm(payForm, target.unpaid_amount);
  payError.value = error ?? '';
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  paying.value = true;
  try {
    await payExpense(target.id, toPayPayload(payForm));
    MessagePlugin.success('付款登记成功（已生成付款凭证）');
    payVisible.value = false;
    await loadRows();
  } catch (error) {
    MessagePlugin.error(errorText(error));
  } finally {
    paying.value = false;
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailId = ref<string | null>(null);

function openDetail(row: ExpenseReport): void {
  detailId.value = row.id;
  detailVisible.value = true;
}

/** 时间戳 → `MM-DD HH:mm`（后端下发 UTC，转北京时间；解析失败原样返回） */
function shortTime(raw: string | null | undefined): string {
  if (!raw) {
    return '—';
  }
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) {
    return raw;
  }
  const shifted = new Date(date.getTime() + 8 * 60 * 60 * 1000);
  return shifted.toISOString().slice(5, 16).replace('T', ' ');
}

function errorText(error: unknown): string {
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return String(error);
}

onMounted(() => {
  void loadRows();
});
</script>

<style scoped>
.pay-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.tool-row__spacer {
  flex: 1;
}

.tool-row__tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
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
  gap: 2px;
}

.money {
  font-variant-numeric: tabular-nums;
  font-family: var(--td-font-family-number, monospace);
}

.money--strong {
  font-weight: 600;
}

.money--warn {
  color: var(--td-warning-color);
}

.muted {
  color: var(--td-text-color-secondary);
}

.mono {
  font-family: var(--td-font-family-number, monospace);
}

.pay-head {
  margin-bottom: 12px;
  padding: 10px 12px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;
}

.pay-head__row {
  display: flex;
  gap: 10px;
  line-height: 1.9;
}

.pay-head__label {
  min-width: 76px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.pay-form {
  margin-top: 4px;
}
</style>
