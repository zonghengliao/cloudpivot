<template>
  <div class="payable-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-payable-refresh" @click="refresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="payable-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.supplier_id"
          class="tool-row__supplier"
          :options="supplierOptions"
          placeholder="全部供应商"
          clearable
          filterable
          data-testid="payable-filter-supplier"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.payable_no"
          class="tool-row__no"
          placeholder="应付单号（模糊）"
          clearable
          data-testid="payable-filter-no"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-payable-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-payable-reset" @click="handleReset">重置</t-button>
      </div>
      <!-- O4 列表记忆：应付单经常按「某供应商 + 某到期区间」反复看，恢复时要让用户看得见 -->
      <ListMemoryBar
        :restored="listMemoryRestored"
        :text="listMemoryText"
        testid="payable-memory"
        @clear="handleClearListMemory"
      />
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">应付单</span>
          <span class="panel-head__meta" data-testid="payable-meta">共 {{ total }} 条</span>
        </div>
        <!-- ★ P7：付款 / 作废已开放（不再是只读页） -->
        <span class="panel-head__tip">付款可多次登记（部分付款）；只有「未付且一分未付」的单能作废</span>
      </div>

      <t-alert
        v-if="apiMissing"
        theme="info"
        message="应付查询接口尚未上线"
        class="list-alert"
        data-testid="payable-api-missing"
      >
        <template #description>
          应付单在收货过账时已自动生成（<span class="mono">biz_payable</span>），但
          <span class="mono">GET /business/erp/payable/page</span> 后端还没注册
          （P2-C1 只交了数据层）。接口上线后本页无需改动即可显示数据。
        </template>
      </t-alert>

      <t-alert
        v-else-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="payable-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadPayables()">重试</t-button>
        </template>
      </t-alert>

      <!-- D7：批量操作工具栏（勾选 → 进度 → 逐条结果） -->
      <BatchActionBar
        :selected="selectedRows"
        :actions="BATCH_ACTIONS"
        :filtered-total="total"
        :runner="runBatchAction"
        :can="canBatchAction"
      :fetch-all-ids="fetchAllRowIds"
        testid="payable-batch"
        @update:selected="onSelectedRows"
        @done="handleBatchDone"
      />

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        :selected-row-keys="selectedIds"
        data-testid="payable-table"
        @select-change="onSelectChange"
        @page-change="handlePageChange"
      >
        <template #payable_no="{ row }">
          <span class="mono" :data-testid="`payable-no-${row.payable_no}`">{{ row.payable_no }}</span>
        </template>

        <template #supplier_id="{ row }">{{ supplierNameOf(row.supplier_id) }}</template>

        <template #source="{ row }">
          <span class="muted" :data-testid="`payable-source-${row.payable_no}`">
            {{ sourceText(row) }}
          </span>
        </template>

        <template #amount="{ row }">
          <span class="money" :data-testid="`payable-amount-${row.payable_no}`">
            {{ formatMoney(row.amount) }}
          </span>
        </template>

        <template #paid_amount="{ row }">
          <span class="money muted">{{ formatMoney(row.paid_amount) }}</span>
        </template>

        <template #unpaid_amount="{ row }">
          <span class="money" :data-testid="`payable-unpaid-${row.payable_no}`">
            {{ formatMoney(unpaidOf(row)) }}
          </span>
          <t-tag v-if="paidPercent(row.paid_amount, row.amount) > 0" size="small" variant="light" theme="primary">
            已付 {{ paidPercent(row.paid_amount, row.amount) }}%
          </t-tag>
        </template>

        <template #due_date="{ row }">
          <span class="muted">{{ shortDate(row.due_date) }}</span>
          <t-tag v-if="overdueText(row)" size="small" variant="light" theme="danger">
            {{ overdueText(row) }}
          </t-tag>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            :theme="payableStatusTheme(row.status)"
            :data-testid="`payable-status-${row.payable_no}`"
          >
            {{ payableStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <div class="op-cell">
            <t-link theme="primary" :data-testid="`btn-payable-view-${row.payable_no}`" @click="openDetail(row)">
              查看
            </t-link>
            <t-link
              v-if="canPay(row)"
              theme="primary"
              :data-testid="`btn-payable-pay-${row.payable_no}`"
              @click="openPay(row)"
            >
              付款
            </t-link>
            <t-link
              v-if="canCancel(row)"
              theme="danger"
              :data-testid="`btn-payable-cancel-${row.payable_no}`"
              @click="openCancel(row)"
            >
              作废
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="payable-empty">
            <t-icon name="file-add" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的应付单' : '暂无应付单' }}</p>
            <p class="panel-empty__desc">
              {{
                filtered
                  ? '换个状态 / 供应商 / 单号再试'
                  : '收货单过账后会自动生成应付单（同一事务），本页只读'
              }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="620px"
      :footer="false"
      :header="false"
      data-testid="payable-drawer"
    >
      <div v-if="detail" class="drawer" data-testid="payable-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title mono">{{ detail.payable_no }}</span>
            <t-tag size="small" :theme="payableStatusTheme(detail.status)">
              {{ payableStatusLabel(detail.status) }}
            </t-tag>
            <t-tag v-if="detail.voucher_id" size="small" variant="outline" theme="success">已生成凭证</t-tag>
          </div>
          <div class="drawer-head__side">
            <span class="muted">{{ sourceText(detail) }}</span>
            <t-button size="small" variant="outline" data-testid="btn-payable-drawer-close" @click="detailVisible = false">
              关闭
            </t-button>
          </div>
        </div>

        <div class="drawer-body">
          <!-- ★ D5：发起付款（payInfo.can_pay 由后端 payment-info 接口裁决，前端不推状态） -->
          <div class="drawer-actions" data-testid="payable-drawer-actions">
            <t-button
              v-if="canStartPay(payInfo?.payable, hasPayPermission)"
              theme="primary"
              data-testid="btn-payable-drawer-pay"
              @click="openPayFromDetail"
            >
              发起付款（未付 {{ formatMoney(payPrefillAmount(payInfo?.payable)) }}）
            </t-button>
            <span
              v-else-if="payInfo && !payInfo.payable.can_pay"
              class="drawer-actions__blocked"
              data-testid="payable-pay-blocked"
            >
              {{ payInfo.payable.blocked_reason }}
            </span>
            <span v-else-if="payInfo && !hasPayPermission" class="drawer-actions__blocked" data-testid="payable-pay-noperm">
              当前账号没有付款权限（business:erp:payable:pay）
            </span>
          </div>
          <!-- ★ D5 反向：审批中那笔现在看已经超付（审批期间被别的付款付掉了） -->
          <t-alert
            v-if="payInfo?.payable.overpay_risk"
            theme="warning"
            :message="payInfo.payable.overpay_risk"
            data-testid="payable-overpay-risk"
          />
          <!-- 可付信息接口失败：只降级「发起付款」区，详情照常看 -->
          <t-alert
            v-if="payInfoError"
            theme="error"
            :message="`取可付信息失败（付款仍可在列表行操作）：${payInfoError}`"
            data-testid="payable-payinfo-error"
          />

          <div class="field-grid">
            <div class="field">
              <span class="field__label">供应商</span>
              <span class="field__value">{{ supplierNameOf(detail.supplier_id) }}</span>
            </div>
            <div class="field">
              <span class="field__label">来源单号</span>
              <span class="field__value mono">
                <t-link
                  v-if="sourcePath"
                  theme="primary"
                  :data-testid="`payable-source-${detail.payable_no}`"
                  @click="openSourceDoc"
                >
                  {{ dash(detail.source_no) }}
                </t-link>
                <template v-else>{{ dash(detail.source_no) }}</template>
              </span>
            </div>
            <div class="field">
              <span class="field__label">应付金额（含税）</span>
              <span class="field__value money" data-testid="payable-detail-amount">{{ formatMoney(detail.amount) }}</span>
            </div>
            <div class="field">
              <span class="field__label">已付金额</span>
              <span class="field__value money" data-testid="payable-detail-paid">{{ formatMoney(detail.paid_amount) }}</span>
            </div>
            <div class="field">
              <span class="field__label">未付金额</span>
              <span class="field__value money" data-testid="payable-detail-unpaid">{{
                formatMoney(unpaidOf(detail))
              }}</span>
            </div>
            <div class="field">
              <span class="field__label">账期（天）</span>
              <span class="field__value">{{ detail.credit_days }}</span>
            </div>
            <div class="field">
              <span class="field__label">到期日</span>
              <span class="field__value">
                {{ dash(detail.due_date) }}
                <t-tag v-if="overdueText(detail)" size="small" variant="light" theme="danger">
                  {{ overdueText(detail) }}
                </t-tag>
              </span>
            </div>
            <div class="field">
              <span class="field__label">会计期间</span>
              <span class="field__value mono">{{ dash(detail.period_code) }}</span>
            </div>
            <div class="field">
              <span class="field__label">发票号</span>
              <span class="field__value">{{ dash(detail.invoice_no) }}</span>
            </div>
            <div class="field">
              <span class="field__label">关联凭证</span>
              <span class="field__value mono">{{ detail.voucher_id ? shortId(detail.voucher_id) : '—' }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">备注</span>
              <span class="field__value">{{ dash(detail.remark) }}</span>
            </div>
          </div>

          <!-- ★ P7：付款记录时间线（部分付款 → 多条） -->
          <div class="payments" data-testid="payable-payments">
            <div class="payments__head">
              <span class="payments__title">付款记录</span>
              <span class="payments__meta" data-testid="payable-payments-count">
                共 {{ detail.payments?.length ?? 0 }} 笔
              </span>
            </div>
            <t-timeline v-if="(detail.payments?.length ?? 0) > 0" class="payments__body">
              <t-timeline-item
                v-for="item in detail.payments"
                :key="item.id"
                :label="shortDate(item.pay_date)"
                :data-testid="`payable-payment-${item.payment_no}`"
              >
                <div class="payment-row">
                  <span class="money">{{ formatMoney(item.amount) }}</span>
                  <span class="mono muted">{{ item.payment_no }}</span>
                  <!-- P17：审批状态标签（审批中=橙色，与「已生效」在视觉上一眼分开） -->
                  <t-tag
                    size="small"
                    variant="light"
                    :theme="paymentTagTheme(item.approval_status)"
                    :data-testid="`payable-payment-status-${item.payment_no}`"
                  >
                    {{ item.approval_status_label }}
                  </t-tag>
                  <span class="muted">{{ item.pay_method || '—' }}</span>
                  <!-- ★ D5 F3：付款记录挂回应付单号 + 付款凭证，都能点回去 -->
                  <t-link
                    v-if="payableLink"
                    theme="primary"
                    hover="color"
                    :data-testid="`payable-payment-payable-${item.payment_no}`"
                    @click="openPayableDoc"
                  >
                    应付单 {{ detail.payable_no }}
                  </t-link>
                  <t-link
                    v-if="item.voucher_no"
                    theme="primary"
                    hover="color"
                    :data-testid="`payable-payment-voucher-${item.payment_no}`"
                    @click="openVoucherDoc(item.voucher_no)"
                  >
                    凭证 {{ item.voucher_no }}
                  </t-link>
                  <!-- 驳回 / 终止的付款记录：可原样重新提交（金额带回来，省得再填） -->
                  <t-link
                    v-if="item.approval_status === 'REJECTED' || item.approval_status === 'CANCELLED'"
                    theme="primary"
                    hover="color"
                    :data-testid="`payable-payment-retry-${item.payment_no}`"
                    @click="openRetry(item)"
                  >
                    重新提交
                  </t-link>
                </div>
                <div v-if="item.remark" class="payment-row__remark">{{ item.remark }}</div>
              </t-timeline-item>
            </t-timeline>
            <p v-else class="payments__empty" data-testid="payable-payments-empty">
              还没有付款记录（未付 {{ formatMoney(unpaidOf(detail)) }}）
            </p>
          </div>

          <!-- ★ P17：付款审批进度（复用审批中心的 FlowProgress，进度只来自引擎写的节点行） -->
          <template v-if="approvalNodes.length > 0">
            <div class="section-title">付款审批进度</div>
            <FlowProgress :nodes="approvalNodes" data-testid="payable-approval-progress" />
          </template>

          <p v-if="canCancel(detail) || canStartPay(payInfo?.payable, hasPayPermission)" class="drawer-note">
            详情里可直接「发起付款」（金额预填<strong>未付</strong>，不是应付总额）。配了「应付付款审批」流程时，付款只是
            <strong>发起审批</strong>（账不动，审批通过才生成「借 2202 应付账款 / 贷 1002 银行存款」的凭证）；
            没配流程则立即生效。付款记录里的「应付单号 / 凭证」都能点回去。
          </p>
        </div>
      </div>
    </t-drawer>

    <!-- ==================== ④ 付款弹窗（可多次，部分付款） ==================== -->
    <t-dialog
      v-model:visible="payVisible"
      header="登记付款"
      width="520px"
      :confirm-btn="{ content: '确认付款', loading: paySubmitting, theme: 'primary' }"
      :close-on-overlay-click="!paySubmitting"
      data-testid="payable-pay-dialog"
      @confirm="submitPay"
    >
      <div v-if="payTarget" class="dialog-body">
        <div class="dialog-meta">
          <span class="mono">{{ payTarget.payable_no }}</span>
          <span class="muted">未付余额 {{ formatMoney(unpaidOf(payTarget)) }}</span>
        </div>
        <t-form label-align="top">
          <t-form-item label="本次付款金额（元）" required-mark>
            <t-input-number
              v-model="payForm.amount"
              theme="normal"
              :min="0"
              :decimal-places="2"
              :step="0.01"
              placeholder="默认 = 未付余额"
              data-testid="payable-pay-amount"
            />
            <p class="form-tip">
              未付余额 {{ formatMoney(unpaidOf(payTarget)) }}；
              一次性付完即结清（SETTLED），否则转「部分已付」。
            </p>
          </t-form-item>
          <t-form-item label="付款日期">
            <t-input v-model="payForm.pay_date" placeholder="YYYY-MM-DD（留空 = 今天）" data-testid="payable-pay-date" />
          </t-form-item>
          <t-form-item label="付款方式">
            <t-select
              v-model="payForm.pay_method"
              :options="payMethodOptions"
              clearable
              placeholder="银行转账 / 支票 / 现金"
              data-testid="payable-pay-method"
            />
          </t-form-item>
          <t-form-item label="备注">
            <t-input v-model="payForm.remark" placeholder="选填" data-testid="payable-pay-remark" />
          </t-form-item>
        </t-form>
        <t-alert v-if="payError" theme="error" :message="payError" data-testid="payable-pay-error" />
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 作废弹窗（原因必填） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      header="作废应付单"
      theme="warning"
      width="480px"
      :confirm-btn="{ content: '确认作废', loading: cancelSubmitting, theme: 'danger' }"
      :close-on-overlay-click="!cancelSubmitting"
      data-testid="payable-cancel-dialog"
      @confirm="submitCancel"
    >
      <div v-if="cancelTarget" class="dialog-body">
        <div class="dialog-meta">
          <span class="mono">{{ cancelTarget.payable_no }}</span>
          <span class="muted">应付 {{ formatMoney(cancelTarget.amount) }}</span>
        </div>
        <t-form label-align="top">
          <t-form-item label="作废原因" required-mark>
            <t-input
              v-model="cancelForm.reason"
              placeholder="例如：重复入库 / 供应商取消订单"
              data-testid="payable-cancel-reason"
            />
          </t-form-item>
        </t-form>
        <p class="form-tip">
          只有「未付（OPEN）且一分钱都没付」的单能作废；已付过款的请走付款冲销。
          作废会同时红冲这张应付单的进项凭证。
        </p>
        <t-alert v-if="cancelError" theme="error" :message="cancelError" data-testid="payable-cancel-error" />
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import {
  payableBatchExport,
} from '@/api/business/batch';
import BatchActionBar from '@/components/BatchActionBar.vue';
import { useBatchSelection } from '@/composables/useBatchSelection';
import { useUserStore } from '@/stores/user';
import type { BatchActionSpec, BatchExportTask, BatchResult, BatchRow } from '@/utils/batch-action';
/**
 * 应付单页（P2-E2）——**只读**列表 + 详情抽屉。
 *
 * # 字段来源（硬红线 1：以接口真实字段名为准）
 *
 * 契约在 `@/api/business/erp` 的 `PayableRow`，逐字抄
 * `backend/src/business/erp/payable.rs` 的 `COLUMNS` / `PayableRow`
 * （`payable_no` / `supplier_id` / `amount` / `paid_amount` / `unpaid_amount` /
 * `status` / `due_date` / `period_code` / `voucher_id` …），不猜 `code` / `name` 那套。
 *
 * # ⚠️ 后端缺口（P2-C1 没交查询接口）
 *
 * `GET /business/erp/payable/page` 在 `backend/src/business/erp/` 里**不存在**
 * （`grep -rn 'erp/payable' backend/src/` 0 命中），V0148 也没种
 * `business:erp:payable:*` 权限码。所以本页按「接口未上线」的姿势处理：
 *
 * * 404 → 顶部蓝色**信息**条 + 空态（不是红色错误，路由未注册也是 404，红错会让人以为页面坏了）；
 * * 其它错误（403 / 500）→ 红色错误条 + 重试。
 *
 * 接口按架构文档 §1888 的契约上线后，本页**不需要改一行**。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { listDictItems } from '@/api/support/dict-api';
import ListMemoryBar from '@/components/ListMemoryBar.vue';
import { useListFilterMemory } from '@/composables/useListFilterMemory';
import {
  fetchPayablePage,
  fetchSupplierOptions,
  type PayableRow,
  type Supplier,
} from '@/api/business/erp';
import {
  cancelPayable,
  fetchPayableDetail,
  fetchPayablePaymentInfo,
  payPayable,
  type PayableDetail,
  type PayablePayment,
  type PayablePaymentInfo,
} from './payable-write-api';
import FlowProgress, {
  type FlowProgressNode,
} from '@/views/business/flow/components/FlowProgress.vue';
import { getFlowInstance } from '@/api/business/flow-api';
import {
  PAYABLE_PAGE_SIZE,
  PAYABLE_STATUS_FILTER_OPTIONS,
  buildPayableQuery,
  canStartPay,
  emptyPayableFilter,
  isPayableFiltered,
  overdueText,
  paidPercent,
  payableDocPath,
  payableStatusLabel,
  payableStatusTheme,
  payPrefillAmount,
  sourceDocPath,
  sourceText,
  supplierNameOf as supplierNameFromMap,
  unpaidOf,
  voucherDocPath,
  type PayableFilterState,
} from './payable-logic';
import { dash, formatCents, formatMoney, shortDate, shortId, toCents, toNumber } from '../shared/money';

/** 主数据下拉一次拉多少（种子规模下够用；上千条应该走远端搜索） */
const OPTION_LIMIT = 200;

/** 表格列（`colKey` 与 `PayableRow` 字段同名，插槽按 colKey 命中） */
const columns: PrimaryTableCol[] = [
  // D7：勾选列（没有任何可用批量动作的行，复选框直接禁用）
  {
    colKey: 'row-select',
    type: 'multiple',
    width: 50,
    disabled: (options: { row: unknown }) => !isSelectable(options.row as PayableRow),
  },
  { colKey: 'payable_no', title: '应付单号', width: 220 },
  { colKey: 'supplier_id', title: '供应商', minWidth: 150, ellipsis: true },
  { colKey: 'source', title: '来源', width: 200, ellipsis: true },
  { colKey: 'amount', title: '应付金额', width: 130, align: 'right' },
  { colKey: 'paid_amount', title: '已付', width: 110, align: 'right' },
  { colKey: 'unpaid_amount', title: '未付', width: 150, align: 'right' },
  { colKey: 'due_date', title: '到期日', width: 130 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 90, fixed: 'right' },
];

/** 状态下拉（`''` = 全部） */
const statusFilterOptions = PAYABLE_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));

// ==================== 列表 ====================

const filter = reactive<PayableFilterState>(emptyPayableFilter());
const rows = ref<PayableRow[]>([]);
const total = ref(0);

// ==================== 批量操作（D7） ====================

/** 一次「选中全部筛选结果」拉多少主键（后端单页上限 200）。 */
const BATCH_FETCH_PAGE_SIZE = 200;

/**
 * 批量动作声明。
 *
 * `allowedStatus` 与后端状态机**逐字一致**：不符合的行由组件禁用按钮并说明原因，
 * 高危动作（作废 / 删除）走二次确认 + 必填原因。
 */
const BATCH_ACTIONS: BatchActionSpec[] = [
  { key: 'export', label: '批量导出', theme: 'default', exportKind: true },
];

/** 动作 → 权限码（与单条按钮同一套码；没有码的按钮禁用并说明）。 */
const BATCH_PERMISSIONS: Record<string, string> = {
  export: 'business:impexp:export:create',
};

const { selectedIds, selectedRows, onSelectChange, onSelectedRows, isSelectable } =
  useBatchSelection(BATCH_ACTIONS, rows);

/** 当前用户能不能点这个批量动作。 */
function canBatchAction(action: BatchActionSpec): boolean {
  const code = BATCH_PERMISSIONS[action.key];
  return !code || userStore.hasPermission(code);
}

/**
 * 执行器：组件按 20 条一批回调这里（一批一个请求）。
 *
 * 后端逐条独立事务：一批里某条失败不影响同批其它条，失败原因逐条回传。
 */
async function runBatchAction(
  _action: BatchActionSpec,
  ids: string[],
  _payload: { reason?: string },
): Promise<BatchResult | BatchExportTask> {
  // 应付单没有「关键词」字段（列表按单号 / 供应商 / 到期日筛），导出沿用单号筛选口径
  return payableBatchExport({ ids, keyword: filter.payable_no || undefined });
}

/** 「选中全部 N 条筛选结果」：按当前筛选把全部主键拉回来（最多 1 万条）。 */
async function fetchAllRowIds(): Promise<BatchRow[]> {
  const acc: BatchRow[] = [];
  for (let page = 1; page <= 50; page += 1) {
    const data = await fetchPayablePage(buildPayableQuery(filter, page, BATCH_FETCH_PAGE_SIZE));
    const list = data.list ?? [];
    acc.push(...list.map((row) => ({ id: row.id, status: row.status })));
    if (list.length === 0 || acc.length >= (data.total ?? 0)) {
      break;
    }
  }
  return acc;
}

/** 批量跑完：回到第一页刷新（勾选已由组件清空，避免误点第二次）。 */
function handleBatchDone(): void {
  void loadPayables(1);
}
const current = ref(1);
const pageSize = PAYABLE_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');
/**
 * 列表记忆（O4）：筛选 + 页码，进页面自动恢复，改动防抖 300ms 写回 sessionStorage。
 * 构造即恢复，早于 `onMounted` 的首次加载。
 */
const {
  restored: listMemoryRestored,
  restoredText: listMemoryText,
  forget: forgetListMemory,
} = useListFilterMemory<PayableFilterState>({
  listName: 'erp-payable',
  filter,
  page: current,
  pageSize,
});
/** 接口未注册（404）：不报红，改成信息条 + 空态 */
const apiMissing = ref(false);

const suppliers = ref<Supplier[]>([]);

const filtered = computed(() => isPayableFiltered(filter));

const supplierOptions = computed(() =>
  suppliers.value.map((item) => ({ label: `${item.supplier_name}（${item.supplier_code}）`, value: item.id })),
);

/** id → 名称（列表译名走 Map，避免每格 find 一遍 200 条） */
const supplierNameMap = computed(() => {
  const map = new Map<string, string>();
  for (const item of suppliers.value) {
    map.set(item.id, item.supplier_name);
  }
  return map;
});

const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 供应商名称（主数据没加载完时退化成短 id） */
function supplierNameOf(supplierId: string | null | undefined): string {
  return supplierNameFromMap(supplierId, supplierNameMap.value);
}

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 是否「接口不存在」。
 *
 * 未注册路由返回 `{code:404,"msg":"请求的资源不存在"}`（HTTP 404），
 * 业务 404 是 `code=10002`；两者都算「接口没上线」。
 */
function isApiMissing(error: unknown): boolean {
  if (error instanceof ApiError) {
    if (error.code === 404) {
      return true;
    }
    return error.message.includes('404') || error.message.includes('请求的资源不存在');
  }
  return false;
}

/** 拉列表 */
async function loadPayables(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchPayablePage(buildPayableQuery(filter, page, pageSize));
    apiMissing.value = false;
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    const rawPage = data.page ?? page;
    const maxPage = Math.max(1, Math.ceil(total.value / pageSize));
    current.value = Math.min(Math.max(1, rawPage), maxPage);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    apiMissing.value = isApiMissing(error);
    loadError.value = apiMissing.value ? '' : messageOf(error);
  } finally {
    loading.value = false;
  }
}

/** 主数据（失败不挡列表：下拉为空时页面还能看单） */
async function loadBasics(): Promise<void> {
  suppliers.value = await fetchSupplierOptions(OPTION_LIMIT).catch(() => [] as Supplier[]);
  // 结算方式走字典（ERP-S1 预置 7 项）；拿不到就留兜底常量，弹窗照样能选
  await listDictItems('pay_method')
    .then((items) => {
      const options = items
        .filter((item) => Number(item.status) !== 0)
        .map((item) => ({ label: item.item_label, value: item.item_value }));
      if (options.length > 0) {
        payMethodOptions.value = options;
      }
    })
    .catch(() => {
      payMethodOptions.value = [...DEFAULT_PAY_METHODS];
    });
}

/** 查询（回第一页） */
function handleSearch(): void {
  void loadPayables(1);
}

/** 重置筛选 */
function handleReset(): void {
  Object.assign(filter, emptyPayableFilter());
  void loadPayables(1);
}

/** 列表记忆 · 「清空筛选」：条件回全量 + 页码回 1 + 删掉 sessionStorage 记忆 */
function handleClearListMemory(): void {
  Object.assign(filter, emptyPayableFilter());
  current.value = 1;
  forgetListMemory();
  void loadPayables(1);
}

/** 刷新（保留筛选与页码） */
function refresh(): void {
  void loadPayables();
}

/** 翻页 */
function handlePageChange(pageInfo: { current?: number }): void {
  void loadPayables(pageInfo.current ?? 1);
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detail = ref<PayableDetail | null>(null);

/**
 * 打开详情：**走接口**（`GET /payable/{id}`）而不是直接复用列表行。
 *
 * 列表行没有 `payments`（付款记录时间线），而抽屉要显示「谁在什么时候付了多少」。
 * 详情接口失败不关抽屉：单头仍可看（用列表行占位）。
 */
async function openDetail(row: PayableRow): Promise<void> {
  detail.value = { ...row, voucher_no: null, payments: [] };
  detailVisible.value = true;
  detailLoading.value = true;
  try {
    detail.value = await fetchPayableDetail(row.id);
    // P17：有审批中的付款 → 顺手把审批进度拉出来（失败不影响详情展示）
    const pending = detail.value.payments.find((item) => item.approval_instance_id);
    await loadApprovalProgress(pending?.approval_instance_id ?? null);
  } catch (error) {
    void MessagePlugin.error(messageOf(error));
  } finally {
    detailLoading.value = false;
  }
  // ★ D5：可付信息（能不能付 / 默认金额 / 超付提示）—— 单独一个只读接口，失败不挡详情
  await loadPayInfo(row.id);
}

// ==================== D5：可付信息（应付单 → 发起付款） ====================

/** 付款权限码（按钮「显不显示」= 后端 `can_pay` **且** 有本码） */
const PAY_PERMISSION = 'business:erp:payable:pay';

const userStore = useUserStore();
const router = useRouter();
const route = useRoute();

/** 当前账号能不能发起付款（按钮 v-if 用；不是置灰） */
const hasPayPermission = computed(() => userStore.hasPermission(PAY_PERMISSION));

/**
 * 可付信息（`GET /business/erp/payable/{id}/payment-info`）。
 *
 * 卡片 F2/O1 的**唯一数据源**：`default_amount` 是后端算好的「未付金额」，
 * `can_pay` / `blocked_reason` 是后端裁决 —— 前端不再按状态自己推一遍（两套口径必然漂移）。
 */
const payInfo = ref<PayablePaymentInfo | null>(null);
/** 可付信息拉取失败的原因（只影响「发起付款」区，详情照常展示） */
const payInfoError = ref('');

/** 拉可付信息（幂等：每次打开详情 / 付款后刷新都调它） */
async function loadPayInfo(id: string): Promise<void> {
  payInfoError.value = '';
  try {
    payInfo.value = await fetchPayablePaymentInfo(id);
  } catch (error) {
    payInfo.value = null;
    payInfoError.value = messageOf(error);
  }
}

/** 来源单据跳转路径（`STOCK_IN` → 收货单详情；其它来源没有页面就返回 null） */
const sourcePath = computed(() => {
  const row = detail.value;
  return row ? sourceDocPath(row.source_type, row.source_id) : null;
});

/** 付款记录上的「应付单号」点回路径（深链 `?payable_no=`） */
const payableLink = computed(() => payableDocPath(detail.value?.payable_no ?? null));

/** 打开来源单据（收货单） */
function openSourceDoc(): void {
  if (sourcePath.value) {
    void router.push(sourcePath.value);
  }
}

/** 打开付款凭证（凭证页支持 `?voucher_no=` 预填筛选） */
function openVoucherDoc(voucherNo: string | null | undefined): void {
  const path = voucherDocPath(voucherNo);
  if (path) {
    void router.push(path);
  }
}

/** 点回应付单（付款记录的来源追溯） */
function openPayableDoc(): void {
  if (payableLink.value) {
    void router.push(payableLink.value);
  }
}

/**
 * 抽屉里「发起付款」：**金额预填后端给的未付金额**（F2 / O1），不是应付总额。
 *
 * 后端 `can_pay = false` 时按钮根本不显示，所以这里不再判状态；
 * 万一 `payment-info` 拉失败（`payInfo = null`），退回列表行的未付金额，用户仍能付。
 */
function openPayFromDetail(): void {
  if (!detail.value) {
    return;
  }
  openPay(detail.value);
  const prefill = payPrefillAmount(payInfo.value?.payable);
  if (prefill > 0) {
    payForm.amount = prefill;
  }
}

// ==================== 付款审批进度（P17） ====================

/** 审批进度节点（`FlowProgress` 的最小形状，只来自后端节点行）。 */
const approvalNodes = ref<FlowProgressNode[]>([]);

/**
 * 拉审批进度。
 *
 * 直接复用审批中心的 `getFlowInstance` + `FlowProgress`：
 * **进度只来自引擎写的节点行**，前端不自己猜「走到第几步」。
 * 拉不到（权限 / 实例已归档）就退化为不显示，不让详情抽屉整块失败。
 */
async function loadApprovalProgress(instanceId: string | null): Promise<void> {
  if (!instanceId) {
    approvalNodes.value = [];
    return;
  }
  try {
    const instance = await getFlowInstance(instanceId);
    approvalNodes.value = (instance.nodes ?? []).map((node) => ({
      node_key: node.node_key,
      name: node.node_name || node.node_key,
      type: node.node_type,
      round: node.round,
      status: node.status,
      approver_count: node.approver_count ?? 0,
    }));
  } catch {
    approvalNodes.value = [];
  }
}

/** 付款记录的审批状态配色：审批中=橙色告警，已通过=绿色，驳回=红色，终止=灰。 */
function paymentTagTheme(status: string): 'warning' | 'success' | 'danger' | 'default' {
  switch (status) {
    case 'PENDING':
      return 'warning';
    case 'APPROVED':
      return 'success';
    case 'REJECTED':
      return 'danger';
    case 'CANCELLED':
      return 'default';
    default:
      return 'success';
  }
}

// ==================== 付款 / 作废（P7） ====================

/** 可付款：未付 / 部分已付（后端同样按这个口径判 409）。 */
function canPay(row: PayableRow | null): boolean {
  if (!row) {
    return false;
  }
  return row.status === 'OPEN' || row.status === 'PARTIAL_PAID';
}

/** 可作废：未付（OPEN）且一分钱都没付（后端同样按这个口径判 409）。 */
function canCancel(row: PayableRow | null): boolean {
  if (!row) {
    return false;
  }
  return row.status === 'OPEN' && toNumber(row.paid_amount) === 0;
}

/**
 * 结算方式下拉（ERP-S1）：标准 7 项来自字典 `pay_method`（迁移 V0187 种下），
 * 拉不到字典时用下面这份同内容的兜底常量 —— 弹窗不能因为字典接口抽风就没有选项。
 *
 * `item_value` 与 `biz_payment.pay_method` 存的中文原串一致（后端是自由文本列，
 * 字典刻意没引入 CASH / BANK 这类代码，免得老数据与新数据两套值）。
 */
const DEFAULT_PAY_METHODS = [
  { label: '现金', value: '现金' },
  { label: '银行转账', value: '银行转账' },
  { label: '承兑汇票', value: '承兑汇票' },
  { label: '支票', value: '支票' },
  { label: '微信', value: '微信' },
  { label: '支付宝', value: '支付宝' },
  { label: '赊账', value: '赊账' },
];

const payMethodOptions = ref<{ label: string; value: string }[]>([...DEFAULT_PAY_METHODS]);

const payVisible = ref(false);
const paySubmitting = ref(false);
const payError = ref('');
const payTarget = ref<PayableRow | null>(null);
const payForm = reactive({ amount: 0, pay_date: '', pay_method: '', remark: '' });

const cancelVisible = ref(false);
const cancelSubmitting = ref(false);
const cancelError = ref('');
const cancelTarget = ref<PayableRow | null>(null);
const cancelForm = reactive({ reason: '' });

/** 打开付款弹窗：金额默认 = 未付余额（一次付完即可结清）。 */
function openPay(row: PayableRow): void {
  payTarget.value = row;
  payError.value = '';
  payForm.amount = toNumber(unpaidOf(row));
  payForm.pay_date = '';
  payForm.pay_method = '';
  payForm.remark = '';
  payVisible.value = true;
}

/**
 * P17：驳回 / 终止的付款记录「重新提交」——把原金额带回来，重新走一遍审批。
 *
 * 只是把付款弹窗打开并预填（**不自动提交**）：审批阈值、未付余额都可能在期间变过，
 * 让人自己确认一次比后端再报 400 友好。
 */
function openRetry(item: PayablePayment): void {
  if (!detail.value) {
    return;
  }
  openPay(detail.value);
  payForm.amount = toNumber(item.amount);
  payForm.pay_method = item.pay_method ?? '';
  payForm.remark = `重新提交（原付款单 ${item.payment_no}）`;
}

/** 打开作废弹窗。 */
function openCancel(row: PayableRow): void {
  cancelTarget.value = row;
  cancelError.value = '';
  cancelForm.reason = '';
  cancelVisible.value = true;
}

/** 本次付款金额（前端先做一次「> 0 且不超过未付」的礼貌校验，后端仍会兜底）。 */
function validatePay(): string {
  const target = payTarget.value;
  if (!target) {
    return '未选择应付单';
  }
  const cents = toCents(payForm.amount);
  if (cents <= 0) {
    return '本次付款金额必须大于 0';
  }
  const unpaid = toCents(unpaidOf(target));
  if (cents > unpaid) {
    return `本次付款 ${formatCents(cents)} 超过未付余额 ${formatCents(unpaid)}`;
  }
  if (payForm.pay_date && !/^\d{4}-\d{2}-\d{2}$/.test(payForm.pay_date.trim())) {
    return '付款日期必须是 YYYY-MM-DD（或留空 = 今天）';
  }
  return '';
}

/** 提交付款。 */
async function submitPay(): Promise<void> {
  const target = payTarget.value;
  if (!target || paySubmitting.value) {
    return;
  }
  const problem = validatePay();
  if (problem) {
    payError.value = problem;
    return;
  }
  paySubmitting.value = true;
  payError.value = '';
  try {
    const result = await payPayable(target.id, {
      amount: (toCents(payForm.amount) / 100).toFixed(4),
      pay_date: payForm.pay_date.trim() || null,
      pay_method: payForm.pay_method || null,
      remark: payForm.remark.trim() || null,
    });
    payVisible.value = false;
    // ★ P17：配了审批流程 → 只是「已提交审批」，账还没动，绝不能提示「付款成功」
    if (result.outcome === 'PENDING_APPROVAL') {
      void MessagePlugin.info(result.message);
    } else {
      void MessagePlugin.success(
        `${result.message}：${result.detail.payable_no} 当前状态「${payableStatusLabel(result.detail.status)}」`,
      );
    }
    // 列表刷新（未付金额 / 状态可能变了），抽屉里换成最新单头 + 审批进度
    detail.value = result.detail;
    await loadApprovalProgress(result.approval_instance_id);
    // ★ D5：可付信息跟着刷新（未付变了 → 按钮文案 / 能否再付都要重算）
    await loadPayInfo(target.id);
    void loadPayables();
  } catch (error) {
    payError.value = messageOf(error);
  } finally {
    paySubmitting.value = false;
  }
}

/** 提交作废。 */
async function submitCancel(): Promise<void> {
  const target = cancelTarget.value;
  if (!target || cancelSubmitting.value) {
    return;
  }
  const reason = cancelForm.reason.trim();
  if (!reason) {
    cancelError.value = '作废原因必填（会计上作废必须留痕）';
    return;
  }
  cancelSubmitting.value = true;
  cancelError.value = '';
  try {
    const updated = await cancelPayable(target.id, reason);
    cancelVisible.value = false;
    void MessagePlugin.success(`已作废：${updated.payable_no}`);
    detail.value = updated;
    // ★ D5：作废后「发起付款」按钮必须立刻消失（可付信息重拉）
    await loadPayInfo(updated.id);
    void loadPayables();
  } catch (error) {
    cancelError.value = messageOf(error);
  } finally {
    cancelSubmitting.value = false;
  }
}

/**
 * 深链：`/business/erp/payable?payable_no=AP-…`（D5 F3：付款记录 / 凭证页点「应付单号」回来）。
 *
 * 用 `payable_no` 而不是 id：外部（凭证摘要、别人发的链接）手上通常只有单号。
 * 打开后把 query 清掉（`router.replace`），免得刷新页面又弹一次抽屉。
 */
async function openByQuery(): Promise<void> {
  const no = typeof route.query.payable_no === 'string' ? route.query.payable_no.trim() : '';
  if (!no) {
    return;
  }
  filter.payable_no = no;
  await loadPayables(1);
  const row = rows.value.find((item) => item.payable_no === no);
  if (row) {
    await openDetail(row);
  } else {
    void MessagePlugin.warning(`没找到应付单 ${no}（可能已删除或无权限）`);
  }
  const rest = { ...route.query };
  delete rest.payable_no;
  void router.replace({ path: route.path, query: rest });
}

onMounted(() => {
  void loadBasics();
  // 列表记忆恢复的页码要在首次加载就生效
  void loadPayables(current.value).then(() => openByQuery());
});
</script>

<style scoped lang="less">
.payable-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* ==================== 工具栏 ==================== */

.tool-card {
  flex: 0 0 auto;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.tool-row__spacer {
  flex: 1 1 auto;
}

.tool-row__status {
  width: 140px;
}

.tool-row__supplier {
  width: 200px;
}

.tool-row__no {
  width: 220px;
}

/* ==================== 列表 ==================== */

.list-card {
  flex: 1 1 auto;
  min-width: 0;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 10px;
}

.panel-head__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.panel-head__tip {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.list-alert {
  margin-bottom: 10px;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.money {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
  color: var(--td-text-color-primary);
}

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  padding: 24px 0;
}

.panel-empty__icon {
  font-size: 28px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 0;
  font-size: 14px;
  color: var(--td-text-color-secondary);
}

.panel-empty__desc {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* ==================== 抽屉 ==================== */

.drawer {
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 4px 4px 0;
}

.drawer-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--td-component-stroke);
}

.drawer-head__main {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.drawer-head__title {
  font-size: 16px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.drawer-head__side {
  display: flex;
  align-items: center;
  gap: 12px;
}

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px 20px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}

.field--wide {
  grid-column: 1 / -1;
}

.field__label {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.field__value {
  font-size: 14px;
  color: var(--td-text-color-primary);
  word-break: break-all;
}

.drawer-note {
  margin: 4px 0 0;
  padding: 8px 12px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 20px;
}

/* ==================== D5：发起付款（详情抽屉顶部） ==================== */

/* 按钮与「不能付的原因」同一行：能付就是主按钮，不能付就是一句说明（不置灰） */
.drawer-actions {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  padding-bottom: 4px;
}

.drawer-actions__blocked {
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
}

/* ==================== 操作列 / 付款记录 / 弹窗（P7） ==================== */

.op-cell {
  display: flex;
  align-items: center;
  gap: 10px;
}

.payments {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.payments__head {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding-bottom: 8px;
  border-bottom: 1px solid var(--td-component-stroke);
}

.payments__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.payments__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.payments__body {
  padding-left: 4px;
}

.payments__empty {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.payment-row {
  display: flex;
  align-items: baseline;
  gap: 12px;
  flex-wrap: wrap;
  font-size: 13px;
}

.payment-row__remark {
  margin-top: 2px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.dialog-body {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.dialog-meta {
  display: flex;
  align-items: baseline;
  gap: 12px;
  padding: 8px 12px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  font-size: 13px;
}

.form-tip {
  margin: 6px 0 0;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-placeholder);
}
</style>
