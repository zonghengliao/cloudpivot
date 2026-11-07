<!--
  报销单详情抽屉（ARCH-6 §4.10 细节 3 / 4）。

  「我的报销」与「待付款」两页共用：列表行点「详情」打开同一个抽屉，避免两处
  各写一份详情 —— 那必然漂移，最后就是「待付款里看得到预算占用，我的报销里看不到」。

  # 四块必备信息（卡面验收逐条对应）

  1. 明细：每行不含税 / 税率 / 税额 / 含税 / 发票 —— 数字全部来自后端（前端不重算）；
  2. 付款记录：金额 / 日期 / 方式 / 账户 / 凭证号；
  3. 预算占用：部门 / 科目 / 占多少 / 占哪个月（`budget_usages`，§4.10 细节 4）；
  4. 审批轨迹：复用审批中心的 `FlowProgress`，节点只来自引擎写的节点行。

  另加：计提凭证号（`voucher_no`）、金额分级（`tier_label`）、超预算告警、附件面板。

  # 为什么预算占用要单独一块

  「部门 / 科目 / 占多少 / 占哪个月」是财务核对预算的口径：单据上只有总额，
  不知道钱压在哪个部门的哪个科目上就无法解释「为什么这个月预算不够」。
-->
<template>
  <t-drawer
    :visible="modelValue"
    size="820px"
    header="报销单详情"
    :footer="false"
    :close-on-overlay-click="true"
    data-testid="expense-detail-drawer"
    @update:visible="(value: boolean) => emit('update:modelValue', value)"
  >
    <div v-if="loading" class="drawer-loading" data-testid="expense-detail-loading">加载中…</div>

    <t-alert
      v-else-if="loadError"
      theme="error"
      :message="loadError"
      data-testid="expense-detail-error"
    >
      <template #operation>
        <t-button size="small" variant="outline" @click="load">重试</t-button>
      </template>
    </t-alert>

    <div v-else-if="detail && report" class="detail" data-testid="expense-detail-body">
      <!-- ==================== ① 单头 ==================== -->
      <div class="detail__head">
        <div class="detail__title">
          <span class="mono detail__no" data-testid="expense-detail-no">{{ report.report_no }}</span>
          <t-tag size="small" variant="light" :theme="statusTheme(report.status)" data-testid="expense-detail-status">
            {{ statusText(report) }}
          </t-tag>
          <t-tag size="small" variant="outline" :theme="payStatusTheme(report.pay_status)" data-testid="expense-detail-pay-status">
            {{ payStatusText(report) }}
          </t-tag>
          <t-tag v-if="detail.tier_label" size="small" variant="outline" data-testid="expense-detail-tier">
            {{ detail.tier_label }}
          </t-tag>
          <!-- FIX-2：提交时实际生效的槽级预算策略（BLOCK 时单独标红，审计一眼能看出被谁管过） -->
          <t-tag
            v-if="report.budget_mode"
            size="small"
            variant="outline"
            :theme="report.budget_mode === 'BLOCK' ? 'danger' : report.budget_mode === 'OFF' ? 'default' : 'warning'"
            data-testid="expense-detail-budget-mode"
          >
            预算策略 {{ report.budget_mode }}
          </t-tag>
        </div>
        <div class="detail__subject" data-testid="expense-detail-title">{{ report.title }}</div>
      </div>

      <div class="detail__grid">
        <div><span class="label">报销人</span><span class="value">{{ dash(report.reporter_name) }}</span></div>
        <div><span class="label">部门</span><span class="value">{{ dash(report.dept_name) }}</span></div>
        <div><span class="label">费用发生日</span><span class="value">{{ report.expense_date }}</span></div>
        <div><span class="label">会计期间</span><span class="value">{{ report.period_code }}</span></div>
      </div>

      <div class="money-row">
        <div class="money-row__cell">
          <span class="money-row__label">不含税</span>
          <span class="money-row__value">{{ formatMoney(report.total_amount) }}</span>
        </div>
        <div class="money-row__cell">
          <span class="money-row__label">税额</span>
          <span class="money-row__value">{{ formatMoney(report.total_tax) }}</span>
        </div>
        <div class="money-row__cell money-row__cell--strong">
          <span class="money-row__label">价税合计</span>
          <span class="money-row__value" data-testid="expense-detail-total">{{ formatMoney(report.total_with_tax) }}</span>
        </div>
        <div class="money-row__cell">
          <span class="money-row__label">已付 / 未付</span>
          <span class="money-row__value">
            {{ formatMoney(report.paid_amount) }} / {{ formatMoney(report.unpaid_amount) }}
          </span>
        </div>
      </div>

      <t-alert
        v-if="report.over_budget"
        theme="warning"
        class="detail__alert"
        data-testid="expense-detail-over-budget"
        :message="`提交时超预算 ${formatMoney(report.over_amount)}：财务审批链会看到这条标记（预算按部门 + 科目 + 月份占用）`"
      />

      <!--
        FIX-5：未编制预算 ≠ 超预算。该单提交时没有任何生效预算管着它 ——
        标黄说明「钱没被预算管」，与「真超支」的红标分开，别让业务以为自己超了。
      -->
      <t-alert
        v-if="report.missing_budget && !report.over_budget"
        theme="info"
        class="detail__alert"
        data-testid="expense-detail-missing-budget"
        message="提交时未编制预算：本单涉及的预算科目在该期间没有生效的预算额度，因此不按超支处理（也不会占用额度）。如需管控，请财务先编制并审批通过预算。"
      />

      <t-alert
        v-if="report.status === 'CANCELLED' && report.cancel_reason"
        theme="info"
        class="detail__alert"
        :message="`作废原因：${report.cancel_reason}`"
        data-testid="expense-detail-cancel-reason"
      />

      <!--
        FIX-2 ①：预算尚未生效（草稿 / 审批中 / 未到生效日）→ 标黄提示，**不阻断**。
        口径不变（DRAFT 预算本来就不参与控制），这里只解决「用户不知道预算没生效」。
      -->
      <t-alert
        v-for="warning in detail.budget_warnings"
        :key="warning.account_id + warning.budget_no"
        theme="warning"
        class="detail__alert"
        data-testid="expense-detail-budget-warning"
        :message="warning.message"
      />

      <p v-if="report.remark" class="drawer-note">备注：{{ report.remark }}</p>

      <!-- ==================== ② 明细 ==================== -->
      <div class="section-title">
        明细（{{ detail.items.length }} 行）
        <span class="section-title__tip">只录含税；价税由服务端按类型可抵扣性拆分</span>
      </div>
      <t-table
        row-key="id"
        size="small"
        :data="detail.items"
        :columns="itemColumns"
        :pagination="undefined"
        data-testid="expense-detail-items"
      >
        <template #type_name="{ row }">
          <div class="name-cell">
            <span class="name-cell__main">{{ row.type_name }}</span>
            <span class="name-cell__sub">{{ row.type_code }} · {{ row.account_code }}</span>
          </div>
        </template>
        <template #amount="{ row }">
          <span class="money">{{ formatMoney(row.amount) }}</span>
        </template>
        <template #tax_rate="{ row }">
          <span class="muted">{{ rateText(row.tax_rate) }}</span>
        </template>
        <template #tax_amount="{ row }">
          <span class="money" :class="{ 'money--zero': toNumber(row.tax_amount) === 0 && toNumber(row.tax_rate) > 0 }">
            {{ formatMoney(row.tax_amount) }}
          </span>
        </template>
        <template #with_tax_amount="{ row }">
          <span class="money money--strong">{{ formatMoney(row.with_tax_amount) }}</span>
        </template>
        <template #invoice="{ row }">
          <span class="mono">{{ dash(row.invoice_no) }}</span>
          <span v-if="row.invoice_type" class="muted"> · {{ invoiceTypeText(row.invoice_type) }}</span>
        </template>
      </t-table>

      <!-- ==================== ③ 预算占用（§4.10 细节 4） ==================== -->
      <div class="section-title">
        预算占用
        <span class="section-title__tip">部门 / 科目 / 占多少 / 占哪个月</span>
      </div>
      <t-table
        v-if="detail.budget_usages.length"
        row-key="id"
        size="small"
        :data="usageRows"
        :columns="usageColumns"
        :pagination="undefined"
        data-testid="expense-detail-budget-usage"
      >
        <template #amount="{ row }">
          <span class="money">{{ formatMoney(row.amount) }}</span>
        </template>
        <template #status_text="{ row }">
          <t-tag size="small" variant="light" :theme="row.over_budget ? 'danger' : row.status_text === '已释放' ? 'default' : 'primary'">
            {{ row.status_text }}
          </t-tag>
        </template>
        <template #over="{ row }">
          <span v-if="row.over_budget" class="money money--warn">
            超 {{ formatMoney(row.over_amount) }}
          </span>
          <span v-else class="muted">—</span>
        </template>
      </t-table>
      <div v-else class="empty" data-testid="expense-detail-budget-empty">
        暂无预算占用：该单的费用类型未开启预算控制，或尚未提交审批。
      </div>

      <!-- ==================== ④ 付款记录 ==================== -->
      <div class="section-title">付款记录</div>
      <t-table
        v-if="detail.payments.length"
        row-key="id"
        size="small"
        :data="detail.payments"
        :columns="paymentColumns"
        :pagination="undefined"
        data-testid="expense-detail-payments"
      >
        <template #amount="{ row }">
          <span class="money">{{ formatMoney(row.amount) }}</span>
        </template>
        <template #pay_method="{ row }">
          {{ payMethodText(row.pay_method) }}
        </template>
        <template #status="{ row }">
          <t-tag size="small" variant="light" :theme="paymentStatusTheme(row.status)">
            {{ paymentStatusText(row.status) }}
          </t-tag>
        </template>
        <template #voucher_no="{ row }">
          <span class="mono">{{ dash(row.voucher_no) }}</span>
        </template>
      </t-table>
      <div v-else class="empty" data-testid="expense-detail-payments-empty">暂无付款记录</div>

      <!-- ==================== ⑤ 审批轨迹 + 计提凭证 ==================== -->
      <div class="section-title">审批轨迹</div>
      <template v-if="hasApprovalProgress(approvalProgress)">
        <FlowProgress
          :nodes="approvalProgress.nodes"
          :tasks="approvalProgress.tasks"
          data-testid="expense-detail-approval"
        />
      </template>
      <div v-else class="empty" data-testid="expense-detail-approval-empty">
        {{
          report.approval_instance_id
            ? '审批节点加载失败（不影响单据本身），请到审批中心查看实例。'
            : '尚未提交审批（草稿单不挂审批实例）。'
        }}
      </div>

      <div class="voucher-row">
        <span class="label">计提凭证号</span>
        <span class="mono" data-testid="expense-detail-voucher">{{ dash(report.voucher_no) }}</span>
        <span v-if="report.approved_at" class="muted">· 审批通过 {{ formatTime(report.approved_at) }}</span>
        <span v-if="report.submitted_at" class="muted">· 提交 {{ formatTime(report.submitted_at) }}</span>
      </div>

      <!-- ==================== ⑥ 附件 ==================== -->
      <div class="section-title">附件（发票影像 / 报销依据）</div>
      <DocAttachmentPanel
        :doc-type="DOC_KIND.EXPENSE_REPORT"
        :doc-id="report.id"
        default-role="INVOICE"
        testid-prefix="expense-attach"
      />
    </div>
  </t-drawer>
</template>

<script setup lang="ts">
/**
 * 详情抽屉只做「读 + 展示」，动作（提交 / 撤回 / 作废 / 付款）留在列表页的工具条里：
 * 两页共用一份详情，动作各页按自己的角色给（我的报销给提交 / 撤回，待付款给付款登记）。
 */
import { computed, ref, watch } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';

import { fetchExpenseDetail, type ExpenseDetail } from '@/api/business/erp/expense';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import { getFlowInstance } from '@/api/business/flow-api';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import {
  EMPTY_APPROVAL_PROGRESS,
  hasApprovalProgress,
  loadApprovalProgress,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
import { dash, formatMoney, toNumber } from '@/views/business/erp/shared/money';
import {
  budgetUsageRows,
  invoiceTypeText,
  payMethodText,
  payStatusText,
  payStatusTheme,
  paymentStatusText,
  paymentStatusTheme,
  rateText,
  statusText,
  statusTheme,
  type BudgetUsageRow,
} from './expense-logic';

const props = defineProps<{
  /** 抽屉是否可见 */
  modelValue: boolean;
  /** 要看的报销单 id（null = 未选） */
  reportId: string | null;
}>();

const emit = defineEmits<{
  /** 抽屉显隐 */
  (event: 'update:modelValue', value: boolean): void;
}>();

const loading = ref(false);
const loadError = ref('');
const detail = ref<ExpenseDetail | null>(null);
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });

const report = computed(() => detail.value?.report ?? null);

const itemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '#', width: 42 },
  { colKey: 'type_name', title: '费用类型 / 科目', width: 180, ellipsis: true },
  { colKey: 'summary', title: '摘要', minWidth: 120, ellipsis: true },
  { colKey: 'expense_date', title: '发生日', width: 96 },
  { colKey: 'amount', title: '不含税', width: 96, align: 'right' },
  { colKey: 'tax_rate', title: '税率', width: 64, align: 'right' },
  { colKey: 'tax_amount', title: '税额', width: 88, align: 'right' },
  { colKey: 'with_tax_amount', title: '含税', width: 96, align: 'right' },
  { colKey: 'invoice', title: '发票', width: 150, ellipsis: true },
];

const usageColumns: PrimaryTableCol[] = [
  { colKey: 'dept_name', title: '部门', width: 130, ellipsis: true },
  { colKey: 'account_text', title: '科目', width: 170, ellipsis: true },
  { colKey: 'amount', title: '占用金额', width: 110, align: 'right' },
  { colKey: 'period_text', title: '占用月份', width: 96 },
  { colKey: 'status_text', title: '状态', width: 88 },
  { colKey: 'over', title: '超预算', width: 110, align: 'right' },
];

const paymentColumns: PrimaryTableCol[] = [
  { colKey: 'payment_no', title: '付款单号', width: 170, ellipsis: true },
  { colKey: 'amount', title: '金额', width: 100, align: 'right' },
  { colKey: 'pay_date', title: '付款日', width: 96 },
  { colKey: 'pay_method', title: '方式', width: 84 },
  { colKey: 'pay_account_code', title: '账户', width: 76 },
  { colKey: 'status', title: '状态', width: 84 },
  { colKey: 'voucher_no', title: '凭证号', width: 150, ellipsis: true },
];

/** 预算占用行：部门名取单头（同一单只压一个部门），科目名取明细快照 */
const usageRows = computed<BudgetUsageRow[]>(() => {
  if (!detail.value) {
    return [];
  }
  const accountNames = new Map<string, string>();
  for (const item of detail.value.items) {
    accountNames.set(item.account_id, item.account_name);
  }
  const deptName = (id: string): string =>
    id === detail.value?.report.dept_id ? detail.value?.report.dept_name ?? '' : '';
  return budgetUsageRows(detail.value.budget_usages, deptName, (id) => accountNames.get(id) ?? '');
});

/** 时间戳 → `yyyy-MM-dd HH:mm`（后端下发 UTC，转成北京时间） */
function formatTime(raw: string | null | undefined): string {
  if (!raw) {
    return '';
  }
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) {
    return raw;
  }
  const shifted = new Date(date.getTime() + 8 * 60 * 60 * 1000);
  return shifted.toISOString().slice(0, 16).replace('T', ' ');
}

async function load(): Promise<void> {
  if (!props.reportId) {
    detail.value = null;
    return;
  }
  loading.value = true;
  loadError.value = '';
  approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
  try {
    const data = await fetchExpenseDetail(props.reportId);
    detail.value = data;
    // 审批轨迹：拉不到就退化（不弹窗、不让详情整块失败）
    approvalProgress.value = await loadApprovalProgress(data.report.approval_instance_id, getFlowInstance);
  } catch (error) {
    detail.value = null;
    loadError.value = error instanceof Error ? error.message : String(error);
  } finally {
    loading.value = false;
  }
}

// 打开 / 换单时重载；关闭不清空（下次打开会覆盖），避免关闭瞬间白屏
watch(
  () => [props.modelValue, props.reportId] as const,
  ([visible, id]) => {
    if (visible && id) {
      void load();
    }
  },
  { immediate: true },
);
</script>

<style scoped>
.drawer-loading {
  padding: 48px 0;
  text-align: center;
  color: var(--td-text-color-secondary);
}

.detail__head {
  margin-bottom: 12px;
}

.detail__title {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.detail__no {
  font-size: 15px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.detail__subject {
  margin-top: 6px;
  font-size: 16px;
  color: var(--td-text-color-primary);
}

.detail__grid {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 8px 16px;
  padding: 12px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;
}

.detail__grid .label,
.voucher-row .label {
  display: inline-block;
  min-width: 64px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.detail__grid .value {
  color: var(--td-text-color-primary);
}

.money-row {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 8px;
  margin: 12px 0;
}

.money-row__cell {
  display: flex;
  flex-direction: column;
  padding: 8px 10px;
  border: 1px solid var(--td-component-border);
  border-radius: 6px;
}

.money-row__cell--strong {
  border-color: var(--td-brand-color);
}

.money-row__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.money-row__value {
  margin-top: 2px;
  font-size: 16px;
  font-family: var(--td-font-family-number, monospace);
  font-variant-numeric: tabular-nums;
}

.money {
  font-variant-numeric: tabular-nums;
  font-family: var(--td-font-family-number, monospace);
}

.money--strong {
  font-weight: 600;
}

.money--zero {
  color: var(--td-warning-color);
}

.money--warn {
  color: var(--td-error-color);
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

.section-title {
  margin: 20px 0 8px;
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.section-title__tip {
  margin-left: 8px;
  font-size: 12px;
  font-weight: 400;
  color: var(--td-text-color-secondary);
}

.voucher-row {
  margin-top: 16px;
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.empty {
  padding: 12px;
  border: 1px dashed var(--td-component-border);
  border-radius: 6px;
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.mono {
  font-family: var(--td-font-family-number, monospace);
}

.muted {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.detail__alert {
  margin: 8px 0;
}

.drawer-note {
  margin: 8px 0;
  color: var(--td-text-color-secondary);
  font-size: 13px;
}
</style>
