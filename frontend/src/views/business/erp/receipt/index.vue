<template>
  <div class="rc-page">
    <!-- ==================== ① 筛选条 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-rc-refresh" @click="refresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="rc-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.customerId"
          class="tool-row__customer"
          :options="customerOptions"
          placeholder="全部客户"
          clearable
          filterable
          data-testid="rc-filter-customer"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.method"
          class="tool-row__method"
          :options="methodOptions"
          placeholder="全部方式"
          clearable
          data-testid="rc-filter-method"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.receiptNo"
          class="tool-row__no"
          placeholder="收款单号 / 客户名（模糊）"
          clearable
          data-testid="rc-filter-no"
          @enter="handleSearch"
        />
      </div>
      <div class="tool-row tool-row--second">
        <span class="tool-row__label">收款日期</span>
        <t-date-picker
          v-model="filter.dateFrom"
          class="tool-row__date"
          value-type="YYYY-MM-DD"
          allow-input
          clearable
          placeholder="起（含）"
          data-testid="rc-filter-date-from"
          @change="handleSearch"
        />
        <span class="tool-row__tilde">~</span>
        <t-date-picker
          v-model="filter.dateTo"
          class="tool-row__date"
          value-type="YYYY-MM-DD"
          allow-input
          clearable
          placeholder="止（含）"
          data-testid="rc-filter-date-to"
          @change="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-rc-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-rc-reset" @click="handleReset">重置</t-button>
        <t-tag v-if="isReceiptFiltered(filter)" size="small" variant="light" theme="primary">
          已筛选
        </t-tag>

        <span class="tool-row__spacer" />

        <t-tooltip :content="createTip" :disabled="!createTip" placement="top-right">
          <span class="op-wrap">
            <t-button
              theme="primary"
              :disabled="!hasCreatePermission"
              data-testid="btn-rc-create"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              收款登记
            </t-button>
          </span>
        </t-tooltip>
      </div>
    </t-card>

    <!-- ==================== ② 收款单列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">收款单</span>
          <span class="panel-head__meta" data-testid="rc-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">
          登记（DRAFT）→ 核销到应收 → 确认 → 过账生成「借 1002 / 贷 1122」凭证；
          <span class="mono">allocated_amount</span> 以库内核销明细 SUM 为准
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="rc-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadReceipts()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="RECEIPT_COLUMNS"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="rc-table"
        @page-change="handlePageChange"
      >
        <template #receipt_no="{ row }">
          <t-link
            theme="primary"
            hover="color"
            :data-testid="`rc-no-${row.receipt_no}`"
            @click="openDetail(row)"
          >
            <span class="mono">{{ row.receipt_no }}</span>
          </t-link>
        </template>

        <template #customer_name="{ row }">
          <span :class="{ muted: !row.customer_name }" :title="row.customer_name || row.customer_id">
            {{ row.customer_name || `（客户已删除 ${shortId(row.customer_id)}）` }}
          </span>
        </template>

        <template #receipt_date="{ row }">
          <span :data-testid="`rc-date-${row.receipt_no}`">{{ dash(row.receipt_date) }}</span>
        </template>

        <template #amount="{ row }">
          <span class="money money--strong">{{ formatMoney(row.amount) }}</span>
        </template>

        <template #allocated_amount="{ row }">
          <span
            class="money"
            :class="{ muted: toCents(row.allocated_amount) === 0 }"
            :data-testid="`rc-allocated-${row.receipt_no}`"
          >
            {{ formatMoney(row.allocated_amount) }}
            <small class="money__pct">{{ allocatedPercent(row) }}%</small>
          </span>
        </template>

        <template #unallocated_amount="{ row }">
          <span
            class="money"
            :class="{ 'money--zero': toCents(row.unallocated_amount) === 0 }"
            :data-testid="`rc-unallocated-${row.receipt_no}`"
          >
            {{ formatMoney(row.unallocated_amount) }}
          </span>
        </template>

        <template #method="{ row }">
          <span :data-testid="`rc-method-${row.receipt_no}`">{{ receiptMethodLabel(row) }}</span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="receiptStatusTheme(row.status)"
            :title="`状态编码：${row.status}`"
            :data-testid="`rc-status-${row.receipt_no}`"
          >
            {{ receiptStatusLabel(row) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <t-button
            size="small"
            variant="text"
            :data-testid="`rc-op-detail-${row.receipt_no}`"
            @click="openDetail(row)"
          >
            详情
          </t-button>

          <t-button
            v-if="isAllocatable(row.status)"
            size="small"
            variant="text"
            :disabled="!hasAllocatePermission"
            :title="hasAllocatePermission ? '' : `缺少权限 ${RECEIPT_PERMS.allocate}`"
            :data-testid="`rc-op-allocate-${row.receipt_no}`"
            @click="openAllocation(row)"
          >
            核销
          </t-button>

          <t-tooltip
            v-if="row.status === 'DRAFT'"
            :content="confirmTip(row)"
            :disabled="!confirmTip(row)"
            placement="top-left"
          >
            <span class="op-wrap">
              <t-button
                size="small"
                variant="text"
                :disabled="!hasStatusPermission"
                :data-testid="`rc-op-confirm-${row.receipt_no}`"
                @click="confirmReceipt(row)"
              >
                确认
              </t-button>
            </span>
          </t-tooltip>

          <t-tooltip
            v-if="row.status === 'CONFIRMED'"
            :content="postBlockedReason(row) || postTip"
            :disabled="!postBlockedReason(row) && !postTip"
            placement="top-left"
          >
            <span class="op-wrap">
              <t-button
                size="small"
                variant="text"
                theme="primary"
                :disabled="!canPostRow(row)"
                :data-testid="`rc-op-post-${row.receipt_no}`"
                @click="postReceipt(row)"
              >
                过账
              </t-button>
            </span>
          </t-tooltip>

          <t-tooltip
            v-if="row.status === 'DRAFT'"
            :content="cancelTip(row)"
            :disabled="!cancelTip(row)"
            placement="top-left"
          >
            <span class="op-wrap">
              <t-button
                size="small"
                variant="text"
                theme="danger"
                :disabled="!canCancelRow(row)"
                :data-testid="`rc-op-cancel-${row.receipt_no}`"
                @click="openCancel(row)"
              >
                作废
              </t-button>
            </span>
          </t-tooltip>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 收款登记弹窗 ==================== -->
    <t-dialog
      v-model:visible="createVisible"
      header="收款登记"
      width="560px"
      :confirm-btn="{ content: '登记收款', loading: createSubmitting, theme: 'primary' }"
      :close-on-overlay-click="!createSubmitting"
      data-testid="rc-create-dialog"
      @confirm="submitCreate"
    >
      <div class="dialog-body">
        <t-alert theme="info" message="登记后为草稿（DRAFT）：先核销到该客户的未收应收，确认后才能过账生成凭证。" />

        <t-form label-align="top" class="form">
          <t-form-item label="客户" required-mark>
            <t-select
              v-model="createForm.customerId"
              :options="customerOptions"
              placeholder="选择客户（收款只能核销同客户的应收）"
              filterable
              clearable
              data-testid="rc-create-customer"
            />
          </t-form-item>
          <t-form-item label="收款金额（元）" required-mark>
            <t-input-number
              v-model="createForm.amount"
              theme="normal"
              :min="0"
              :decimal-places="2"
              :step="0.01"
              placeholder="必须大于 0"
              data-testid="rc-create-amount"
            />
          </t-form-item>
          <t-form-item label="收款日期">
            <t-date-picker
              v-model="createForm.receiptDate"
              value-type="YYYY-MM-DD"
              allow-input
              clearable
              placeholder="留空 = 今天（CST）"
              data-testid="rc-create-date"
            />
          </t-form-item>
          <t-form-item label="收款方式">
            <t-select
              v-model="createForm.method"
              :options="methodOptions"
              clearable
              placeholder="银行转账 / 现金 / 支票 / 其它"
              data-testid="rc-create-method"
            />
          </t-form-item>
          <t-form-item label="收款账户">
            <t-input
              v-model="createForm.bankAccount"
              placeholder="选填：银行账号 / 现金账台"
              data-testid="rc-create-bank"
            />
          </t-form-item>
          <t-form-item label="备注">
            <t-input v-model="createForm.remark" placeholder="选填" data-testid="rc-create-remark" />
          </t-form-item>
        </t-form>
      </div>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="收款单详情"
      size="760px"
      :footer="false"
      data-testid="rc-detail-drawer"
    >
      <t-loading :loading="detailLoading" size="small">
        <t-alert v-if="detailError" theme="error" :message="detailError" data-testid="rc-detail-error">
          <template #operation>
            <t-button size="small" variant="outline" @click="reloadDetail()">重试</t-button>
          </template>
        </t-alert>

        <template v-if="detail">
          <div class="detail-head">
            <span class="detail-head__no mono" data-testid="rc-detail-no">{{ detail.receipt_no }}</span>
            <t-tag
              size="small"
              variant="light"
              :theme="receiptStatusTheme(detail.status)"
              data-testid="rc-detail-status"
            >
              {{ receiptStatusLabel(detail) }}
            </t-tag>
            <t-tag
              v-if="toCents(detail.unallocated_amount) > 0 && isAllocatable(detail.status)"
              size="small"
              variant="light"
              theme="warning"
            >
              未核销 {{ formatMoney(detail.unallocated_amount) }}
            </t-tag>
          </div>

          <div class="field-grid">
            <div class="field">
              <span class="field__label">客户</span>
              <span class="field__value" data-testid="rc-detail-customer">
                {{ detail.customer_name || `（客户已删除 ${shortId(detail.customer_id)}）` }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">收款日期</span>
              <span class="field__value">{{ dash(detail.receipt_date) }}</span>
            </div>
            <div class="field">
              <span class="field__label">收款金额</span>
              <span class="field__value money money--strong" data-testid="rc-detail-amount">
                {{ formatMoney(detail.amount) }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">已核销 / 未核销</span>
              <span class="field__value money" data-testid="rc-detail-allocated">
                {{ formatMoney(detail.allocated_amount) }} / {{ formatMoney(detail.unallocated_amount) }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">收款方式</span>
              <span class="field__value">{{ receiptMethodLabel(detail) }}</span>
            </div>
            <div class="field">
              <span class="field__label">收款账户</span>
              <span class="field__value mono">{{ dash(detail.bank_account) }}</span>
            </div>
            <div class="field">
              <span class="field__label">关联凭证</span>
              <span class="field__value mono" data-testid="rc-detail-voucher">
                {{ detail.voucher_no || (detail.voucher_id ? shortId(detail.voucher_id) : '—') }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">过账后余额</span>
              <span class="field__value">{{ postBlockedReason(detail) || '可过账（核销已满）' }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">备注</span>
              <span class="field__value">{{ dash(detail.remark) }}</span>
            </div>
          </div>

          <div class="alloc-section" data-testid="rc-detail-allocations">
            <div class="alloc-section__head">
              <span class="alloc-section__title">核销明细</span>
              <span class="alloc-section__meta" data-testid="rc-detail-alloc-count">
                共 {{ detail.allocations.length }} 张应收 · 合计 {{ formatMoney(detail.allocated_amount) }}
              </span>
            </div>

            <table v-if="detail.allocations.length" class="mini-table">
              <thead>
                <tr>
                  <th>应收单号</th>
                  <th class="mini-table__num">应收金额</th>
                  <th class="mini-table__num">本次核销</th>
                  <th>应收状态</th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="item in detail.allocations"
                  :key="item.id"
                  :data-testid="`rc-detail-alloc-${item.ar_no}`"
                >
                  <td class="mono">{{ item.ar_no }}</td>
                  <td class="mini-table__num">{{ formatMoney(item.ar_amount) }}</td>
                  <td class="mini-table__num money money--strong">{{ formatMoney(item.amount) }}</td>
                  <td>
                    <t-tag size="small" variant="outline">{{ item.ar_status_label || item.ar_status }}</t-tag>
                  </td>
                </tr>
              </tbody>
            </table>
            <p v-else class="alloc-section__empty" data-testid="rc-detail-alloc-empty">
              还没有核销明细（未核销 {{ formatMoney(detail.unallocated_amount) }}）：
              {{ isAllocatable(detail.status) ? '点「核销」把这张收款冲到该客户的应收上' : '已过账 / 已作废的收款不能再核销' }}
            </p>
          </div>

          <div class="detail-actions">
            <t-button
              v-if="isAllocatable(detail.status)"
              theme="primary"
              variant="outline"
              :disabled="!hasAllocatePermission"
              data-testid="rc-detail-allocate"
              @click="openAllocation(detail)"
            >
              核销
            </t-button>
            <t-button
              v-if="detail.status === 'DRAFT'"
              variant="outline"
              :disabled="!hasStatusPermission"
              data-testid="rc-detail-confirm"
              @click="confirmReceipt(detail)"
            >
              确认
            </t-button>
            <t-button
              v-if="detail.status === 'CONFIRMED'"
              theme="primary"
              :disabled="!canPostRow(detail)"
              :title="postBlockedReason(detail)"
              data-testid="rc-detail-post"
              @click="postReceipt(detail)"
            >
              过账
            </t-button>
            <t-button
              v-if="detail.status === 'DRAFT'"
              theme="danger"
              variant="outline"
              :disabled="!canCancelRow(detail)"
              :title="cancelTip(detail)"
              data-testid="rc-detail-cancel"
              @click="openCancel(detail)"
            >
              作废
            </t-button>
            <t-button variant="outline" data-testid="btn-rc-detail-close" @click="detailVisible = false">
              关闭
            </t-button>
          </div>
        </template>
      </t-loading>
    </t-drawer>

    <!-- ==================== ⑤ 核销面板 ==================== -->
    <t-dialog
      v-model:visible="allocVisible"
      header="核销到应收"
      width="920px"
      :footer="false"
      :close-on-overlay-click="!allocSubmitting"
      data-testid="rc-alloc-dialog"
    >
      <div v-if="allocTarget" class="dialog-body alloc" data-testid="rc-alloc-panel">
        <div class="alloc__head">
          <div class="alloc__head-main">
            <span class="mono alloc__no">{{ allocTarget.receipt_no }}</span>
            <span class="muted">{{ allocTarget.customer_name || '（客户已删除）' }}</span>
            <t-tag size="small" variant="light" :theme="receiptStatusTheme(allocTarget.status)">
              {{ receiptStatusLabel(allocTarget) }}
            </t-tag>
          </div>
          <span class="alloc__amounts">
            收款 <b class="money">{{ formatMoney(allocTarget.amount) }}</b>
            · 已核销 <b class="money">{{ formatMoney(allocTarget.allocated_amount) }}</b>
          </span>
        </div>

        <t-alert
          v-if="allocError"
          theme="error"
          :message="allocError"
          class="alloc__alert"
          data-testid="rc-alloc-load-error"
        />

        <div class="alloc__bar">
          <t-button
            size="small"
            variant="outline"
            :disabled="allocLoading || allocRows.length === 0"
            data-testid="btn-rc-alloc-fill"
            @click="fillAllocation"
          >
            按金额顺序填充（先冲最早到期）
          </t-button>
          <t-button
            size="small"
            variant="outline"
            :disabled="allocLoading || allocRows.length === 0"
            data-testid="btn-rc-alloc-clear"
            @click="clearAllocation"
          >
            全部清空
          </t-button>
          <span class="alloc__hint">
            勾选 + 填金额；已核销过的行不能取消（本版本无反核销接口）
          </span>
        </div>

        <t-loading :loading="allocLoading" size="small">
          <table v-if="allocRows.length" class="alloc-table" data-testid="rc-alloc-table">
            <thead>
              <tr>
                <th class="alloc-table__check">核销</th>
                <th>应收单号</th>
                <th>到期日</th>
                <th class="alloc-table__num">应收金额</th>
                <th class="alloc-table__num">未收</th>
                <th class="alloc-table__num">本次核销（元）</th>
                <th class="alloc-table__num">剩余</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="row in allocRows"
                :key="row.receivable_id"
                :class="{ 'alloc-row--locked': row.priorCents > 0 }"
                :data-testid="`rc-alloc-row-${row.ar_no}`"
              >
                <td class="alloc-table__check">
                  <t-checkbox
                    :checked="row.checked"
                    :data-testid="`rc-alloc-check-${row.ar_no}`"
                    @change="(checked: unknown) => handleCheck(row, Boolean(checked))"
                  />
                </td>
                <td>
                  <span class="mono">{{ row.ar_no }}</span>
                  <t-tag
                    v-if="row.priorCents > 0"
                    size="small"
                    variant="light"
                    theme="primary"
                    class="alloc-row__prior"
                    :title="`本收款此前已核销 ${formatCents(row.priorCents)}`"
                  >
                    已核销
                  </t-tag>
                </td>
                <td>{{ dash(row.due_date) }}</td>
                <td class="alloc-table__num">{{ formatMoney(row.amount) }}</td>
                <td class="alloc-table__num">{{ formatMoney(row.unsettled_amount) }}</td>
                <td class="alloc-table__num">
                  <t-input-number
                    :model-value="row.allocated / 100"
                    theme="normal"
                    :min="0"
                    :decimal-places="2"
                    :step="0.01"
                    :disabled="!row.checked"
                    size="small"
                    :data-testid="`rc-alloc-input-${row.ar_no}`"
                    @change="(value: unknown) => setAllocatedYuan(row, value)"
                  />
                </td>
                <td class="alloc-table__num">
                  <span
                    class="money"
                    :class="{ 'text-danger': rowRemainingCents(row) < 0 }"
                    :data-testid="`rc-alloc-remain-${row.ar_no}`"
                  >
                    {{ formatCents(rowRemainingCents(row)) }}
                  </span>
                </td>
              </tr>
            </tbody>
          </table>

          <p v-else-if="!allocLoading && !allocError" class="alloc__empty" data-testid="rc-alloc-empty">
            该客户没有可核销的应收（只有「未收 / 部分已收」且仍有未收金额的应收能收款）
          </p>
        </t-loading>

        <div class="alloc__summary" data-testid="rc-alloc-summary">
          <span>
            收款金额
            <b class="money" data-testid="rc-alloc-sum-receipt">{{ formatCents(allocSummary.receiptCents) }}</b>
          </span>
          <span>
            本次核销
            <b class="money" data-testid="rc-alloc-sum-allocated">{{ formatCents(allocSummary.allocatedCents) }}</b>
            <small class="muted">（{{ allocSummary.checkedCount }} 张）</small>
          </span>
          <span>
            剩余
            <b
              class="money"
              :class="{ 'text-danger': allocSummary.remainingCents < 0 }"
              data-testid="rc-alloc-sum-remaining"
            >
              {{ formatCents(allocSummary.remainingCents) }}
            </b>
          </span>
        </div>

        <p class="alloc__error" data-testid="rc-alloc-error">{{ allocValidation || '' }}</p>

        <div class="alloc__actions">
          <t-button
            theme="primary"
            :loading="allocSubmitting"
            :disabled="allocSubmitting || allocLoading || allocRows.length === 0"
            data-testid="btn-rc-alloc-submit"
            @click="submitAllocation"
          >
            保存核销
          </t-button>
          <t-button variant="outline" :disabled="allocSubmitting" @click="allocVisible = false">关闭</t-button>
        </div>
      </div>
    </t-dialog>

    <!-- ==================== ⑥ 作废弹窗（原因必填） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      header="作废收款单"
      theme="warning"
      width="480px"
      :confirm-btn="{ content: '确认作废', loading: cancelSubmitting, theme: 'danger' }"
      :close-on-overlay-click="!cancelSubmitting"
      data-testid="rc-cancel-dialog"
      @confirm="submitCancel"
    >
      <div v-if="cancelTarget" class="dialog-body">
        <div class="dialog-meta">
          <span class="mono">{{ cancelTarget.receipt_no }}</span>
          <span class="muted">收款 {{ formatMoney(cancelTarget.amount) }}</span>
        </div>
        <t-form label-align="top" class="form">
          <t-form-item label="作废原因" required-mark>
            <t-textarea
              v-model="cancelReason"
              :maxlength="RECEIPT_REASON_MAX"
              :autosize="{ minRows: 3, maxRows: 6 }"
              placeholder="例如：客户退票 / 金额登记错误，重新登记"
              data-testid="rc-cancel-reason"
            />
          </t-form-item>
        </t-form>
        <p class="form-tip">
          只有「草稿 + 一分钱都没核销」的收款能作废；作废是不可逆终态
          （状态图里没有 <span class="mono">CANCELLED→*</span> 这条边）。
        </p>
        <p class="alloc__error" data-testid="rc-cancel-error">{{ cancelErrorText }}</p>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 收款单页（SO-18）—— 后端 SO-17（`backend/src/business/erp/receipt.rs`，5 个接口）。
 *
 * # 口径来源（逐字抄后端，不猜）
 *
 * | 语义 | 出处 |
 * |------|------|
 * | 列表 / 详情行 | `receipt::ReceiptRes`（`@/api/business/erp` 的 `Receipt` / `ReceiptDetail`） |
 * | 分页入参 | `receipt::ReceiptPageQuery`（`receipt_no` 是**模糊**匹配单号+客户名；空串一律不发） |
 * | 状态域 4 值 | `receipt::STATUSES` = DRAFT / CONFIRMED / POSTED / CANCELLED（**不是**应收那套） |
 * | 状态机 | `state_machine::RECEIPT_EDGES`（3 条边，没有 CONFIRMED→CANCELLED 与 POSTED→*） |
 * | 核销 | `POST /receipt/{id}/allocate`：单张 ≤ 应收可核销余额，合计 ≤ 收款金额 |
 * | 过账 | `PATCH /receipt/{id}/status` → POSTED，**要求核销满**（`allocated_amount == amount`），否则 409 |
 * | 作废 | DRAFT→CANCELLED，原因 ≥ `receipt::REASON_MIN_CHARS`（4 字，按字符数） |
 *
 * # 为什么"实时剩余"必须走整数分
 *
 * 后端 `Decimal` 出 JSON 是字符串（`"1000.0000"`）。核销面板是**会算错钱**的地方，
 * 勾选 / 累加 / 比较全部经 `views/business/erp/shared/money.ts` 的 `toCents`，
 * 只在显示前换回元（`formatCents`）。纯逻辑与单测在 `./receipt-logic.ts`。
 *
 * # 核销面板为什么只列「未收应收」还要两次分页
 *
 * `GET /receivable/page` 的 `status` 只收单值，而「未收」在后端是 OPEN 与 PARTIAL 两种
 * （`receivable::has_balance`）。所以打两次再本地按 id 去重、按到期日升序 ——
 * 财务拿到一笔钱的默认动作是「先冲最早的账」，`fillSequentially` 依赖这个顺序。
 *
 * # 已核销的行为什么不能取消
 *
 * 后端 `allocate` 只做 UPSERT，**不删**不在本次 items 里的行；而单行金额必须 > 0。
 * 反核销接口 SO-17 未开放，所以「取消勾选」会让前端显示与库内不一致：
 * 逻辑层 `priorAllocationError` 直接把口子堵住（已有核销金额的行只能改大或不动）。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';

import {
  allocateReceipt,
  changeReceiptStatus,
  createReceipt,
  fetchCustomerPage,
  fetchReceiptDetail,
  fetchReceiptPage,
  fetchReceivablePage,
  type Customer,
  type Receipt,
  type ReceiptDetail,
  type ReceiptMethod,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';

import { dash, formatCents, formatMoney, shortId, toCents, todayString } from '../shared/money';
import {
  ALLOCATION_FETCH_LIMIT,
  RECEIPT_COLUMNS,
  RECEIPT_METHOD_OPTIONS,
  RECEIPT_PAGE_SIZE,
  RECEIPT_PERMS,
  RECEIPT_REASON_MAX,
  RECEIPT_STATUS_OPTIONS,
  allocationSummary,
  allocatableCents,
  allocatedPercent,
  buildAllocateItems,
  buildAllocationRows,
  buildReceiptQuery,
  canCancel,
  canConfirm,
  canPost,
  cancelBlockedReason,
  createFormError,
  emptyReceiptFilter,
  fillSequentially,
  hasAllocationChanges,
  isAllocatable,
  isReceiptFiltered,
  mergeAllocatableReceivables,
  postBlockedReason,
  receiptMethodLabel,
  receiptReasonError,
  receiptStatusLabel,
  receiptStatusTheme,
  rowRemainingCents,
  validateAllocation,
  type AllocationRowState,
  type ReceiptFilterState,
} from './receipt-logic';

const userStore = useUserStore();

/** 主数据下拉一次拉多少（种子规模够用；后端上限 200） */
const OPTION_LIMIT = 200;

const statusOptions = RECEIPT_STATUS_OPTIONS.map((item) => ({ ...item }));
const methodOptions = RECEIPT_METHOD_OPTIONS.map((item) => ({ ...item }));

const hasCreatePermission = computed(() => userStore.hasPermission(RECEIPT_PERMS.create));
const hasAllocatePermission = computed(() => userStore.hasPermission(RECEIPT_PERMS.allocate));
const hasStatusPermission = computed(() => userStore.hasPermission(RECEIPT_PERMS.status));

const createTip = computed(() =>
  hasCreatePermission.value ? '' : `缺少权限 ${RECEIPT_PERMS.create}`,
);
const postTip = computed(() => (hasStatusPermission.value ? '' : `缺少权限 ${RECEIPT_PERMS.status}`));

// ==================== 列表 ====================

const filter = reactive<ReceiptFilterState>(emptyReceiptFilter());
const rows = ref<Receipt[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = RECEIPT_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

const customers = ref<Customer[]>([]);
const customerOptions = computed(() =>
  customers.value.map((item) => ({ label: `${item.name}（${item.code}）`, value: item.id })),
);

const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

function messageOf(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return error instanceof Error ? error.message : String(error);
}

async function loadReceipts(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchReceiptPage(buildReceiptQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    const rawPage = data.page ?? page;
    const maxPage = Math.max(1, Math.ceil(total.value / pageSize));
    current.value = Math.min(Math.max(1, rawPage), maxPage);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = messageOf(error);
  } finally {
    loading.value = false;
  }
}

async function loadCustomers(): Promise<void> {
  customers.value = await fetchCustomerPage({ page: 1, page_size: OPTION_LIMIT })
    .then((data) => data.list ?? [])
    .catch(() => [] as Customer[]);
}

function handleSearch(): void {
  void loadReceipts(1);
}

function handleReset(): void {
  Object.assign(filter, emptyReceiptFilter());
  void loadReceipts(1);
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  void loadReceipts(pageInfo.current);
}

function refresh(): void {
  void loadReceipts(current.value);
}

// ==================== 权限相关的按钮态 ====================

/** 确认按钮的置灰原因（权限 / 状态） */
function confirmTip(row: Pick<Receipt, 'status'>): string {
  if (!hasStatusPermission.value) return `缺少权限 ${RECEIPT_PERMS.status}`;
  if (!canConfirm(row)) return `当前状态 ${row.status} 不能确认`;
  return '';
}

function canPostRow(row: Pick<Receipt, 'status' | 'amount' | 'allocated_amount'>): boolean {
  return hasStatusPermission.value && canPost(row);
}

function canCancelRow(row: Pick<Receipt, 'status' | 'allocated_amount'>): boolean {
  return hasStatusPermission.value && canCancel(row);
}

/** 作废按钮的置灰原因（权限优先，其次状态 / 已核销） */
function cancelTip(row: Pick<Receipt, 'status' | 'allocated_amount'>): string {
  if (!hasStatusPermission.value) return `缺少权限 ${RECEIPT_PERMS.status}`;
  return cancelBlockedReason(row);
}

// ==================== 收款登记 ====================

const createVisible = ref(false);
const createSubmitting = ref(false);

const createForm = reactive<{
  customerId: string;
  amount: number;
  receiptDate: string;
  method: ReceiptMethod | '';
  bankAccount: string;
  remark: string;
}>({
  customerId: '',
  amount: 0,
  receiptDate: todayString(),
  method: '',
  bankAccount: '',
  remark: '',
});

function openCreate(): void {
  if (!hasCreatePermission.value) {
    MessagePlugin.warning(createTip.value);
    return;
  }
  Object.assign(createForm, {
    customerId: '',
    amount: 0,
    receiptDate: todayString(),
    method: '',
    bankAccount: '',
    remark: '',
  });
  createVisible.value = true;
}

async function submitCreate(): Promise<void> {
  const localError = createFormError({
    customer_id: createForm.customerId,
    amount: createForm.amount,
    receipt_date: createForm.receiptDate,
  });
  if (localError) {
    MessagePlugin.warning(localError);
    return;
  }
  createSubmitting.value = true;
  try {
    const created = await createReceipt({
      customer_id: createForm.customerId,
      receipt_date: createForm.receiptDate || null,
      // 后端 Decimal 收字符串：定点 4 位，避免 0.1 的二进制尾巴
      amount: (toCents(createForm.amount) / 100).toFixed(4),
      method: createForm.method || null,
      bank_account: createForm.bankAccount.trim() || null,
      remark: createForm.remark.trim() || null,
    });
    MessagePlugin.success(`${created.receipt_no} 已登记（草稿），下一步核销到应收`);
    createVisible.value = false;
    await loadReceipts(1);
    // 登记完直接开核销面板：收款登记的下一个动作 99% 是把它冲到应收上
    await openAllocation(created);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    createSubmitting.value = false;
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<ReceiptDetail | null>(null);
const detailId = ref('');

async function openDetail(row: Receipt): Promise<void> {
  detailId.value = row.id;
  detail.value = null;
  detailVisible.value = true;
  await reloadDetail();
}

async function reloadDetail(): Promise<void> {
  if (!detailId.value) return;
  detailLoading.value = true;
  detailError.value = '';
  try {
    detail.value = await fetchReceiptDetail(detailId.value);
  } catch (error) {
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

/** 动作成功后统一收口：详情（若开着）就地换新 + 列表刷新 */
async function afterMutation(updated: ReceiptDetail): Promise<void> {
  if (detailVisible.value && detailId.value === updated.id) {
    detail.value = updated;
  }
  if (allocVisible.value && allocTarget.value?.id === updated.id) {
    allocTarget.value = updated;
  }
  await loadReceipts(current.value);
}

// ==================== 确认 / 过账 ====================

const statusBusy = ref('');

async function runStatus(
  row: Pick<Receipt, 'id' | 'receipt_no'>,
  status: 'CONFIRMED' | 'POSTED',
  reason?: string,
): Promise<void> {
  statusBusy.value = row.id;
  try {
    const updated = await changeReceiptStatus(row.id, status, reason);
    MessagePlugin.success(
      status === 'POSTED'
        ? `${row.receipt_no} 已过账${updated.voucher_no ? `（凭证 ${updated.voucher_no}）` : ''}`
        : `${row.receipt_no} 已确认，下一步核销到应收`,
    );
    await afterMutation(updated);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    statusBusy.value = '';
  }
}

async function confirmReceipt(row: Pick<Receipt, 'id' | 'receipt_no' | 'status'>): Promise<void> {
  if (!hasStatusPermission.value) {
    MessagePlugin.warning(`缺少权限 ${RECEIPT_PERMS.status}`);
    return;
  }
  if (!canConfirm(row)) {
    MessagePlugin.warning(`当前状态 ${row.status} 不能确认`);
    return;
  }
  await runStatus(row, 'CONFIRMED');
}

function postReceipt(row: Receipt): void {
  if (!canPostRow(row)) {
    MessagePlugin.warning(postBlockedReason(row) || postTip.value || '当前不能过账');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '过账确认',
    body: `${row.receipt_no} 收款 ${formatMoney(row.amount)}，核销已满 ${formatMoney(row.allocated_amount)}。\n过账会生成「借 1002 银行存款 / 贷 1122 应收账款」的凭证，过账后金额与核销被凭证锁定（不可再改）。`,
    confirmBtn: '确认过账',
    theme: 'warning',
    onConfirm: async () => {
      dialog.hide();
      await runStatus(row, 'POSTED');
    },
  });
}

// ==================== 作废 ====================

const cancelVisible = ref(false);
const cancelSubmitting = ref(false);
const cancelTarget = ref<Receipt | null>(null);
const cancelReason = ref('');
const cancelError = ref('');

const cancelErrorText = computed(() => cancelError.value || receiptReasonError(cancelReason.value));

function openCancel(row: Receipt): void {
  if (!canCancelRow(row)) {
    MessagePlugin.warning(cancelTip(row));
    return;
  }
  cancelTarget.value = row;
  cancelReason.value = '';
  cancelError.value = '';
  cancelVisible.value = true;
}

async function submitCancel(): Promise<void> {
  const target = cancelTarget.value;
  if (!target) return;
  const error = receiptReasonError(cancelReason.value);
  if (error) {
    cancelError.value = error;
    return;
  }
  cancelSubmitting.value = true;
  cancelError.value = '';
  try {
    const updated = await changeReceiptStatus(target.id, 'CANCELLED', cancelReason.value.trim());
    MessagePlugin.success(`${target.receipt_no} 已作废`);
    cancelVisible.value = false;
    cancelTarget.value = null;
    await afterMutation(updated);
  } catch (err) {
    cancelError.value = messageOf(err);
    MessagePlugin.error(cancelError.value);
  } finally {
    cancelSubmitting.value = false;
  }
}

// ==================== 核销面板 ====================

const allocVisible = ref(false);
const allocLoading = ref(false);
const allocError = ref('');
const allocSubmitting = ref(false);
const allocTarget = ref<ReceiptDetail | null>(null);
const allocRows = ref<AllocationRowState[]>([]);

const allocSummary = computed(() =>
  allocTarget.value
    ? allocationSummary(allocTarget.value.amount, allocRows.value)
    : { receiptCents: 0, allocatedCents: 0, remainingCents: 0, checkedCount: 0 },
);

const allocValidation = computed(() =>
  allocTarget.value ? validateAllocation(allocTarget.value.amount, allocRows.value) : '',
);

/** 打开面板：拉该客户「未收」应收（OPEN + PARTIAL 两次分页）+ 本收款已有核销明细 */
async function openAllocation(row: Receipt | ReceiptDetail): Promise<void> {
  if (!hasAllocatePermission.value) {
    MessagePlugin.warning(`缺少权限 ${RECEIPT_PERMS.allocate}`);
    return;
  }
  if (!isAllocatable(row.status)) {
    MessagePlugin.warning(`只有草稿 / 已确认的收款能核销（当前 ${receiptStatusLabel(row)}）`);
    return;
  }
  allocTarget.value = 'allocations' in row ? row : null;
  allocRows.value = [];
  allocError.value = '';
  allocVisible.value = true;
  allocLoading.value = true;
  try {
    const [head, open, partial] = await Promise.all([
      'allocations' in row ? Promise.resolve(row) : fetchReceiptDetail(row.id),
      fetchReceivablePage({
        page: 1,
        page_size: ALLOCATION_FETCH_LIMIT,
        customer_id: row.customer_id,
        status: 'OPEN',
      }),
      fetchReceivablePage({
        page: 1,
        page_size: ALLOCATION_FETCH_LIMIT,
        customer_id: row.customer_id,
        status: 'PARTIAL',
      }),
    ]);
    allocTarget.value = head;
    const merged = mergeAllocatableReceivables([...(open.list ?? []), ...(partial.list ?? [])]);
    allocRows.value = buildAllocationRows(merged, head.allocations);
  } catch (error) {
    allocError.value = messageOf(error);
  } finally {
    allocLoading.value = false;
  }
}

/** 勾选 / 取消勾选（取消已核销过的行 = 不可撤销，直接拦下） */
function handleCheck(row: AllocationRowState, checked: boolean): void {
  if (checked) {
    row.checked = true;
    if (row.allocated <= 0) {
      const others = allocationSummary(
        allocTarget.value?.amount ?? 0,
        allocRows.value.filter((item) => item.receivable_id !== row.receivable_id),
      ).remainingCents;
      const cap = allocatableCents(row);
      row.allocated = Math.max(0, Math.min(cap, others));
    }
    return;
  }
  if (row.priorCents > 0) {
    MessagePlugin.warning(
      `「${row.ar_no}」已核销 ${formatCents(row.priorCents)}：本版本没有反核销接口，不能取消勾选（要冲回请走红冲）`,
    );
    return;
  }
  row.checked = false;
  row.allocated = 0;
}

/** 面板里填金额（元）→ 分 */
function setAllocatedYuan(row: AllocationRowState, value: unknown): void {
  const cents = toCents(value as number | string | null | undefined);
  row.allocated = cents > 0 ? cents : 0;
  if (row.allocated > 0) row.checked = true;
}

/** 按收款金额顺序填满：先冲最早到期的应收（面板已按到期日升序） */
function fillAllocation(): void {
  if (!allocTarget.value) return;
  allocRows.value = fillSequentially(allocTarget.value.amount, allocRows.value);
}

/** 清空：已核销过的行必须保留原金额（无反核销），其余归零 */
function clearAllocation(): void {
  allocRows.value = allocRows.value.map((row) =>
    row.priorCents > 0
      ? { ...row, checked: true, allocated: row.priorCents }
      : { ...row, checked: false, allocated: 0 },
  );
}

async function submitAllocation(): Promise<void> {
  const target = allocTarget.value;
  if (!target) return;
  const error = validateAllocation(target.amount, allocRows.value);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  // 未改金额的行不发：后端对 SETTLED 应收（上一笔已核满）会 409，那行库里本来就对
  if (!hasAllocationChanges(allocRows.value)) {
    MessagePlugin.info('核销金额没有变化，无需保存');
    return;
  }
  allocSubmitting.value = true;
  allocError.value = '';
  try {
    const updated = await allocateReceipt(target.id, buildAllocateItems(allocRows.value));
    MessagePlugin.success(
      `${updated.receipt_no} 已核销 ${formatMoney(updated.allocated_amount)} / ${formatMoney(updated.amount)}`,
    );
    allocTarget.value = updated;
    allocRows.value = buildAllocationRows([], updated.allocations);
    allocVisible.value = false;
    await afterMutation(updated);
  } catch (err) {
    allocError.value = messageOf(err);
    MessagePlugin.error(allocError.value);
  } finally {
    allocSubmitting.value = false;
  }
}

onMounted(() => {
  void loadCustomers();
  void loadReceipts(1);
});
</script>

<style scoped>
.rc-page {
  padding: 16px;
}

.tool-card,
.list-card {
  margin-bottom: 12px;
  border-radius: 8px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.tool-row--second {
  margin-top: 8px;
}

.tool-row__spacer {
  flex: 1;
}

.tool-row__label {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.tool-row__tilde {
  color: var(--td-text-color-placeholder);
}

.tool-row__status {
  width: 130px;
}

.tool-row__customer {
  width: 200px;
}

.tool-row__method {
  width: 130px;
}

.tool-row__no {
  width: 240px;
}

.tool-row__date {
  width: 150px;
}

.panel-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 12px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.panel-head__title {
  font-size: 15px;
  font-weight: 600;
}

.panel-head__meta {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.panel-head__tip {
  margin-left: auto;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.list-alert {
  margin-bottom: 12px;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
  font-size: 12px;
}

.muted {
  color: var(--td-text-color-placeholder);
}

.money {
  font-variant-numeric: tabular-nums;
}

.money--strong {
  font-weight: 600;
}

.money--zero {
  color: var(--td-text-color-placeholder);
}

.money__pct {
  margin-left: 4px;
  color: var(--td-text-color-placeholder);
  font-size: 11px;
}

.text-danger {
  color: var(--td-error-color);
  font-weight: 600;
}

.op-wrap {
  display: inline-block;
}

.form {
  margin-top: 12px;
}

.dialog-body {
  max-height: 64vh;
  overflow-y: auto;
}

.dialog-meta {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 8px;
}

.form-tip {
  margin: 4px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

/* ==================== 详情 ==================== */

.detail-head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 16px;
}

.detail-head__no {
  font-size: 15px;
  font-weight: 600;
}

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px 20px;
  margin-bottom: 20px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.field--wide {
  grid-column: 1 / -1;
}

.field__label {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.field__value {
  color: var(--td-text-color-primary);
  font-size: 13px;
  word-break: break-all;
}

.alloc-section {
  margin-bottom: 20px;
  padding-top: 16px;
  border-top: 1px solid var(--td-component-border);
}

.alloc-section__head {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-bottom: 12px;
}

.alloc-section__title {
  font-size: 14px;
  font-weight: 600;
}

.alloc-section__meta {
  margin-left: auto;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.alloc-section__empty {
  margin: 0;
  padding: 16px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;
}

.mini-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.mini-table th,
.mini-table td {
  padding: 8px 10px;
  border-bottom: 1px solid var(--td-component-border);
  text-align: left;
  white-space: nowrap;
}

.mini-table th {
  color: var(--td-text-color-secondary);
  font-weight: 500;
  background: var(--td-bg-color-container-hover);
}

.mini-table__num {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.detail-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  padding-top: 12px;
  border-top: 1px solid var(--td-component-border);
}

/* ==================== 核销面板 ==================== */

.alloc__head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  flex-wrap: wrap;
}

.alloc__head-main {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.alloc__no {
  font-size: 14px;
  font-weight: 600;
}

.alloc__amounts {
  margin-left: auto;
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.alloc__alert {
  margin-top: 12px;
}

.alloc__bar {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 12px 0;
  flex-wrap: wrap;
}

.alloc__hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.alloc-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.alloc-table th,
.alloc-table td {
  padding: 6px 8px;
  border-bottom: 1px solid var(--td-component-border);
  text-align: left;
  white-space: nowrap;
}

.alloc-table th {
  color: var(--td-text-color-secondary);
  font-weight: 500;
  background: var(--td-bg-color-container-hover);
}

.alloc-table__check {
  width: 48px;
  text-align: center;
}

.alloc-table__num {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.alloc-table :deep(.t-input-number) {
  width: 120px;
}

.alloc-row--locked {
  background: var(--td-bg-color-container-hover);
}

.alloc-row__prior {
  margin-left: 6px;
}

.alloc__empty {
  padding: 24px;
  color: var(--td-text-color-placeholder);
  text-align: center;
}

.alloc__summary {
  display: flex;
  gap: 24px;
  align-items: baseline;
  margin-top: 12px;
  padding: 10px 12px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;
  font-size: 13px;
}

.alloc__error {
  min-height: 18px;
  margin: 6px 0 0;
  color: var(--td-error-color);
  font-size: 12px;
}

.alloc__actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  margin-top: 8px;
}
</style>
