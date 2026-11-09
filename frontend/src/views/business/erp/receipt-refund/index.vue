<template>
  <div class="rf-page">
    <!-- ==================== ① 筛选条 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-rf-refresh" @click="refresh">
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
          data-testid="rf-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.customerId"
          class="tool-row__customer"
          :options="customerOptions"
          placeholder="全部客户"
          clearable
          filterable
          data-testid="rf-filter-customer"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.method"
          class="tool-row__method"
          :options="methodOptions"
          placeholder="全部方式"
          clearable
          data-testid="rf-filter-method"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.refundNo"
          class="tool-row__no"
          placeholder="退款单号 / 客户名（模糊）"
          clearable
          data-testid="rf-filter-no"
          @enter="handleSearch"
        />
      </div>
      <div class="tool-row tool-row--second">
        <span class="tool-row__label">退款日期</span>
        <t-date-picker
          v-model="filter.dateFrom"
          class="tool-row__date"
          value-type="YYYY-MM-DD"
          allow-input
          clearable
          placeholder="起（含）"
          data-testid="rf-filter-date-from"
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
          data-testid="rf-filter-date-to"
          @change="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-rf-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-rf-reset" @click="handleReset">重置</t-button>
        <t-tag v-if="isRefundFiltered(filter)" size="small" variant="light" theme="primary">
          已筛选
        </t-tag>

        <span class="tool-row__spacer" />

        <t-tooltip :content="createTip" :disabled="!createTip" placement="top-right">
          <span class="op-wrap">
            <t-button
              theme="primary"
              :disabled="!hasCreatePermission"
              data-testid="btn-rf-create"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              登记退款
            </t-button>
          </span>
        </t-tooltip>
      </div>
    </t-card>

    <!-- ==================== ② 退款单列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">收款退款单</span>
          <span class="panel-head__meta" data-testid="rf-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">
          登记（DRAFT）→ 核销冲减应收已收 → 确认（CONFIRMED）→ 提交审批 → 通过后自动过账
          （生成「借 1122 应收 / 贷 1002 银行」凭证）；退款金额与冲减明细都不得超出「已收到的钱」
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="rf-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadRefunds(current)">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="REFUND_COLUMNS"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="rf-table"
        @page-change="handlePageChange"
      >
        <template #refund_no="{ row }">
          <t-link
            theme="primary"
            hover="color"
            :data-testid="`rf-no-${row.refund_no}`"
            @click="openDetail(row)"
          >
            <span class="mono">{{ row.refund_no }}</span>
          </t-link>
        </template>

        <template #source_receipt_no="{ row }">
          <span
            class="mono"
            :class="{ muted: !row.source_receipt_no }"
            :title="row.source_receipt_id || ''"
            :data-testid="`rf-src-${row.refund_no}`"
          >
            {{ row.source_receipt_no || '—' }}
          </span>
        </template>

        <template #customer_name="{ row }">
          <span :class="{ muted: !row.customer_name }" :title="row.customer_name || row.customer_id">
            {{ row.customer_name || `（客户已删除 ${shortId(row.customer_id)}）` }}
          </span>
        </template>

        <template #refund_date="{ row }">
          <span :data-testid="`rf-date-${row.refund_no}`">{{ dash(row.refund_date) }}</span>
        </template>

        <template #amount="{ row }">
          <span class="money money--strong" :data-testid="`rf-amount-${row.refund_no}`">
            {{ formatMoney(row.amount) }}
          </span>
        </template>

        <template #allocated_amount="{ row }">
          <span
            class="money"
            :class="{ 'money--zero': toCents(row.allocated_amount) === 0 }"
            :data-testid="`rf-allocated-${row.refund_no}`"
          >
            {{ formatMoney(row.allocated_amount) }}
          </span>
        </template>

        <template #method_label="{ row }">
          <span :data-testid="`rf-method-${row.refund_no}`">{{ row.method_label || '未指定' }}</span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="refundStatusTheme(row.status)"
            :title="`状态编码：${row.status}`"
            :data-testid="`rf-status-${row.refund_no}`"
          >
            {{ row.status_label }}
          </t-tag>
        </template>

        <template #approval_status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="approvalTheme(row.approval_status)"
            :title="row.approval_instance_no || ''"
            :data-testid="`rf-approval-${row.refund_no}`"
          >
            {{ approvalLabel(row.approval_status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <t-button
            size="small"
            variant="text"
            :data-testid="`rf-op-detail-${row.refund_no}`"
            @click="openDetail(row)"
          >
            详情
          </t-button>

          <t-button
            v-if="isAllocatable(row)"
            size="small"
            variant="text"
            :disabled="!hasAllocatePermission"
            :title="hasAllocatePermission ? '' : `缺少权限 ${REFUND_PERMS.allocate}`"
            :data-testid="`rf-op-allocate-${row.refund_no}`"
            @click="openAllocation(row)"
          >
            核销
          </t-button>

          <t-tooltip
            v-if="row.status === 'DRAFT'"
            :content="statusTip(row, canConfirm(row), 'DRAFT → CONFIRMED')"
            :disabled="!statusTip(row, canConfirm(row), 'DRAFT → CONFIRMED')"
            placement="top-left"
          >
            <span class="op-wrap">
              <t-button
                size="small"
                variant="text"
                :disabled="!hasStatusPermission || !canConfirm(row)"
                :data-testid="`rf-op-confirm-${row.refund_no}`"
                @click="confirmRefund(row)"
              >
                确认
              </t-button>
            </span>
          </t-tooltip>

          <t-tooltip
            v-if="row.status === 'CONFIRMED' && row.approval_status !== 'PENDING'"
            :content="statusTip(row, canSubmitApproval(row), submitBlockedReason(row) || '')"
            :disabled="!statusTip(row, canSubmitApproval(row), submitBlockedReason(row) || '')"
            placement="top-left"
          >
            <span class="op-wrap">
              <t-button
                size="small"
                variant="text"
                theme="primary"
                :disabled="!hasStatusPermission || !canSubmitApproval(row)"
                :data-testid="`rf-op-submit-${row.refund_no}`"
                @click="submitApproval(row)"
              >
                提交审批
              </t-button>
            </span>
          </t-tooltip>

          <t-button
            v-if="canWithdraw(row)"
            size="small"
            variant="text"
            :disabled="!hasStatusPermission || withdrawingId === row.id"
            :loading="withdrawingId === row.id"
            :data-testid="`rf-op-withdraw-${row.refund_no}`"
            @click="withdrawApproval(row)"
          >
            撤回
          </t-button>

          <t-tooltip
            v-if="row.status === 'DRAFT'"
            :content="cancelBlockedReason(row) || ''"
            :disabled="!cancelBlockedReason(row)"
            placement="top-left"
          >
            <span class="op-wrap">
              <t-button
                size="small"
                variant="text"
                theme="danger"
                :disabled="!hasStatusPermission || !canCancel(row)"
                :data-testid="`rf-op-cancel-${row.refund_no}`"
                @click="openCancel(row)"
              >
                作废
              </t-button>
            </span>
          </t-tooltip>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 登记退款弹窗 ==================== -->
    <t-dialog
      v-model:visible="createVisible"
      header="登记退款"
      width="600px"
      :confirm-btn="{ content: '登记退款', loading: createSubmitting, theme: 'primary' }"
      :close-on-overlay-click="!createSubmitting"
      data-testid="rf-create-dialog"
      @confirm="submitCreate"
    >
      <div class="dialog-body">
        <t-alert
          theme="info"
          message="登记后是草稿（DRAFT）：先在「核销」里逐张冲减应收的已收金额，确认后才能过账。退款金额只能退已经收到的钱。"
        />

        <t-form label-align="top" class="form">
          <t-form-item label="客户" required-mark>
            <t-select
              v-model="createForm.customerId"
              :options="customerOptions"
              placeholder="选择客户（退款只能冲同客户的应收）"
              filterable
              clearable
              data-testid="rf-create-customer"
              @change="handleCreateCustomerChange"
            />
          </t-form-item>

          <t-form-item label="来源收款单">
            <t-select
              v-model="createForm.sourceReceiptId"
              :options="sourceReceiptOptions"
              :loading="sourceReceiptLoading"
              :disabled="!createForm.customerId"
              placeholder="选填：退的是哪一笔收款（选中后带出可退金额）"
              filterable
              clearable
              data-testid="rf-create-source"
              @change="handleCreateSourceChange"
            />
            <p v-if="capHint" class="form-tip" data-testid="rf-create-cap">{{ capHint }}</p>
          </t-form-item>

          <t-form-item label="退款金额（元）" required-mark>
            <t-input-number
              v-model="createForm.amount"
              theme="normal"
              :min="0"
              :decimal-places="2"
              :step="0.01"
              placeholder="必须大于 0，且不超过可退金额"
              data-testid="rf-create-amount"
            />
          </t-form-item>

          <t-form-item label="退款日期">
            <t-date-picker
              v-model="createForm.refundDate"
              value-type="YYYY-MM-DD"
              allow-input
              clearable
              placeholder="留空 = 今天（CST）"
              data-testid="rf-create-date"
            />
          </t-form-item>

          <t-form-item label="退款方式">
            <t-select
              v-model="createForm.method"
              :options="methodOptions"
              clearable
              placeholder="银行转账 / 现金 / 支票 / 其它"
              data-testid="rf-create-method"
            />
          </t-form-item>

          <t-form-item label="付款账户">
            <t-input
              v-model="createForm.bankAccount"
              placeholder="选填：银行账号 / 现金账台"
              data-testid="rf-create-bank"
            />
          </t-form-item>

          <t-form-item label="退款原因" required-mark>
            <t-textarea
              v-model="createForm.reason"
              :maxlength="REFUND_REASON_MAX"
              :autosize="{ minRows: 2, maxRows: 5 }"
              placeholder="至少 4 个字：客户退货 / 重复收款 / 合同变更…"
              data-testid="rf-create-reason"
            />
            <p class="form-tip">退款是钱往外走，原因必填且会写进状态日志（审计留痕）。</p>
          </t-form-item>

          <t-form-item label="备注">
            <t-input v-model="createForm.remark" placeholder="选填" data-testid="rf-create-remark" />
          </t-form-item>
        </t-form>
      </div>
    </t-dialog>

    <!-- ==================== ④ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="收款退款单详情"
      size="800px"
      :footer="false"
      data-testid="rf-detail-drawer"
    >
      <t-loading :loading="detailLoading" size="small">
        <t-alert v-if="detailError" theme="error" :message="detailError" data-testid="rf-detail-error">
          <template #operation>
            <t-button size="small" variant="outline" @click="reloadDetail()">重试</t-button>
          </template>
        </t-alert>

        <template v-if="detail">
          <div class="detail-head">
            <span class="detail-head__no mono" data-testid="rf-detail-no">{{ detail.refund_no }}</span>
            <t-tag
              size="small"
              variant="light"
              :theme="refundStatusTheme(detail.status)"
              data-testid="rf-detail-status"
            >
              {{ detail.status_label }}
            </t-tag>
            <t-tag
              size="small"
              variant="light"
              :theme="approvalTheme(detail.approval_status)"
              data-testid="rf-detail-approval"
            >
              {{ approvalLabel(detail.approval_status) }}
            </t-tag>
            <t-tag
              v-if="unallocatedCents(detail) > 0 && isAllocatable(detail)"
              size="small"
              variant="light"
              theme="warning"
              data-testid="rf-detail-unallocated"
            >
              未冲减 {{ formatCents(unallocatedCents(detail)) }}
            </t-tag>
          </div>

          <div class="field-grid">
            <div class="field">
              <span class="field__label">客户</span>
              <span class="field__value" data-testid="rf-detail-customer">
                {{ detail.customer_name || `（客户已删除 ${shortId(detail.customer_id)}）` }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">来源收款单</span>
              <span class="field__value mono" data-testid="rf-detail-source">
                {{ detail.source_receipt_no || dash(detail.source_receipt_id) }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">退款日期</span>
              <span class="field__value">{{ dash(detail.refund_date) }}</span>
            </div>
            <div class="field">
              <span class="field__label">退款金额</span>
              <span class="field__value money money--strong" data-testid="rf-detail-amount">
                {{ formatMoney(detail.amount) }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">已冲减 / 未冲减</span>
              <span class="field__value money" data-testid="rf-detail-allocated">
                {{ formatMoney(detail.allocated_amount) }} / {{ formatMoney(detail.unallocated_amount) }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">退款方式</span>
              <span class="field__value">{{ detail.method_label || '未指定' }}</span>
            </div>
            <div class="field">
              <span class="field__label">付款账户</span>
              <span class="field__value mono">{{ dash(detail.bank_account) }}</span>
            </div>
            <div class="field">
              <span class="field__label">退款凭证</span>
              <span class="field__value mono" data-testid="rf-detail-voucher">
                {{ detail.voucher_no || (detail.voucher_id ? shortId(detail.voucher_id) : '—') }}
              </span>
            </div>
            <div class="field field--wide">
              <span class="field__label">退款原因</span>
              <span class="field__value" data-testid="rf-detail-reason">{{ dash(detail.reason) }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">备注</span>
              <span class="field__value">{{ dash(detail.remark) }}</span>
            </div>
          </div>

          <!-- 冲减明细 -->
          <div class="section-title" data-testid="rf-detail-allocations">冲减明细</div>
          <p class="section-tip" data-testid="rf-detail-alloc-count">
            共 {{ detail.allocations.length }} 张应收 · 合计 {{ formatMoney(detail.allocated_amount) }}
            （冲减 = 把该应收的「已收金额」减回来，应收状态随之回退）
          </p>
          <table v-if="detail.allocations.length" class="mini-table">
            <thead>
              <tr>
                <th>应收单号</th>
                <th class="mini-table__num">应收金额</th>
                <th class="mini-table__num">冲减金额</th>
                <th class="mini-table__num">冲减后已收</th>
                <th class="mini-table__num">冲减后未收</th>
                <th>应收状态</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="item in detail.allocations"
                :key="item.id"
                :data-testid="`rf-alloc-row-${item.ar_no}`"
              >
                <td><span class="mono">{{ item.ar_no }}</span></td>
                <td class="mini-table__num">{{ formatMoney(item.ar_amount) }}</td>
                <td class="mini-table__num money money--strong">{{ formatMoney(item.amount) }}</td>
                <td class="mini-table__num">{{ formatMoney(item.ar_received) }}</td>
                <td class="mini-table__num">{{ formatMoney(item.ar_unsettled) }}</td>
                <td>
                  <t-tag size="small" variant="outline">{{ item.ar_status_label }}</t-tag>
                </td>
              </tr>
            </tbody>
          </table>
          <div v-else class="empty" data-testid="rf-detail-alloc-empty">
            还没有冲减明细{{ isAllocatable(detail) ? '：点下方「核销」逐张冲减应收的已收金额' : '' }}
          </div>

          <!-- 审批进度 -->
          <div class="section-title">审批进度</div>
          <template v-if="hasApprovalProgress(approvalProgress)">
            <FlowProgress
              :nodes="approvalProgress.nodes"
              :tasks="approvalProgress.tasks"
              data-testid="rf-detail-flow"
            />
          </template>
          <div v-else class="empty" data-testid="rf-detail-flow-empty">
            {{
              hasApprovalInstance(detail)
                ? '审批节点加载失败（不影响单据本身），请到审批中心查看实例。'
                : '尚未提交审批：确认 + 核销满之后点「提交审批」，审批通过会自动过账。'
            }}
          </div>

          <!-- 附件 -->
          <div class="section-title">附件（退款凭据）</div>
          <p class="section-tip">
            建议上传：银行付款回单 / 客户签收的退款确认。非必传，不传也能走流程。
          </p>
          <DocAttachmentPanel
            :doc-type="DOC_KIND.RECEIPT_REFUND"
            :doc-id="detail.id"
            default-role="BANK_RECEIPT"
            testid-prefix="rf-attach"
          />

          <!-- 详情内操作 -->
          <div class="detail-actions">
            <t-button
              v-if="isAllocatable(detail)"
              theme="primary"
              variant="outline"
              :disabled="!hasAllocatePermission"
              :title="hasAllocatePermission ? '' : `缺少权限 ${REFUND_PERMS.allocate}`"
              data-testid="btn-rf-drawer-allocate"
              @click="openAllocation(detail)"
            >
              核销
            </t-button>
            <t-button
              v-if="detail.status === 'DRAFT'"
              :disabled="!hasStatusPermission || !canConfirm(detail)"
              data-testid="btn-rf-drawer-confirm"
              @click="confirmRefund(detail)"
            >
              确认
            </t-button>
            <t-button
              v-if="detail.status === 'CONFIRMED' && detail.approval_status !== 'PENDING'"
              theme="primary"
              :disabled="!hasStatusPermission || !canSubmitApproval(detail)"
              :title="submitBlockedReason(detail) || ''"
              data-testid="btn-rf-drawer-submit"
              @click="submitApproval(detail)"
            >
              提交审批
            </t-button>
            <t-button
              v-if="canWithdraw(detail)"
              :disabled="!hasStatusPermission || withdrawingId === detail.id"
              :loading="withdrawingId === detail.id"
              data-testid="btn-rf-drawer-withdraw"
              @click="withdrawApproval(detail)"
            >
              撤回审批
            </t-button>
            <t-button
              v-if="detail.status === 'CONFIRMED' && detail.approval_status !== 'PENDING'"
              theme="success"
              :disabled="!hasStatusPermission || !canPost(detail)"
              :title="postBlockedReason(detail) || ''"
              data-testid="btn-rf-drawer-post"
              @click="postRefund(detail)"
            >
              直接过账
            </t-button>
            <t-button
              v-if="detail.status === 'DRAFT'"
              theme="danger"
              variant="outline"
              :disabled="!hasStatusPermission || !canCancel(detail)"
              :title="cancelBlockedReason(detail) || ''"
              data-testid="btn-rf-drawer-cancel"
              @click="openCancel(detail)"
            >
              作废
            </t-button>
          </div>
        </template>
      </t-loading>
    </t-drawer>

    <!-- ==================== ⑤ 核销弹窗 ==================== -->
    <t-dialog
      v-model:visible="allocVisible"
      header="退款核销（冲减应收已收）"
      width="900px"
      :footer="false"
      :close-on-overlay-click="!allocSubmitting"
      data-testid="rf-alloc-dialog"
    >
      <div v-if="allocTarget" class="dialog-body">
        <div class="dialog-meta">
          <span class="mono">{{ allocTarget.refund_no }}</span>
          <span class="muted">
            退款 {{ formatMoney(allocTarget.amount) }} · 已冲减 {{ formatMoney(allocTarget.allocated_amount) }}
            · 未冲减 {{ formatCents(unallocatedCents(allocTarget)) }}
          </span>
        </div>

        <t-loading :loading="allocLoading" size="small">
          <div class="alloc__head">
            <span>勾选应收并填本次冲减金额（只能冲已经收到的钱）</span>
            <t-button size="small" variant="outline" @click="fillAll">按可退金额自动填满</t-button>
          </div>

          <table v-if="allocRows.length" class="mini-table" data-testid="rf-alloc-table">
            <thead>
              <tr>
                <th class="mini-table__check">冲减</th>
                <th>应收单号</th>
                <th class="mini-table__num">应收金额</th>
                <th class="mini-table__num">已收</th>
                <th class="mini-table__num">可退</th>
                <th class="mini-table__num">本次冲减（元）</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="row in allocRows"
                :key="row.receivableId"
                :data-testid="`rf-alloc-row-${row.arNo}`"
              >
                <td class="mini-table__check">
                  <t-checkbox
                    :checked="row.inputAmount !== null"
                    :data-testid="`rf-alloc-check-${row.arNo}`"
                    @change="(checked: unknown) => toggleAllocRow(row, Boolean(checked))"
                  />
                </td>
                <td><span class="mono">{{ row.arNo }}</span></td>
                <td class="mini-table__num">{{ formatCents(row.arAmountCents) }}</td>
                <td class="mini-table__num">{{ formatCents(row.receivedCents) }}</td>
                <td class="mini-table__num">{{ formatCents(refundableCents(row.receivedCents, row.alreadyCents)) }}</td>
                <td class="mini-table__num">
                  <t-input-number
                    :model-value="row.inputAmount ?? undefined"
                    theme="normal"
                    size="small"
                    :min="0"
                    :decimal-places="2"
                    :step="0.01"
                    :disabled="row.inputAmount === null"
                    :data-testid="`rf-alloc-input-${row.arNo}`"
                    @update:model-value="(value: unknown) => setAllocAmount(row, value)"
                  />
                </td>
              </tr>
            </tbody>
          </table>
          <div v-else class="empty" data-testid="rf-alloc-empty">
            该客户没有「已收过钱且还能冲减」的应收 —— 收款退款只能冲已经收到的钱；
            如果这笔钱是刚收的，请先到「收款单」确认核销到应收。
          </div>
        </t-loading>

        <p class="alloc__summary">
          冲减合计 <span class="money money--strong">{{ formatCents(allocTotalCents) }}</span>
          / 退款金额 {{ formatMoney(allocTarget.amount) }}
        </p>
        <p class="alloc__error" data-testid="rf-alloc-error">{{ allocValidation || '' }}</p>

        <div class="alloc__actions">
          <t-button
            theme="primary"
            :loading="allocSubmitting"
            :disabled="allocSubmitting || allocLoading || allocRows.length === 0 || Boolean(allocValidation)"
            data-testid="btn-rf-alloc-submit"
            @click="submitAllocation"
          >
            保存冲减
          </t-button>
          <t-button variant="outline" :disabled="allocSubmitting" @click="allocVisible = false">关闭</t-button>
        </div>
      </div>
    </t-dialog>

    <!-- ==================== ⑥ 作废弹窗（原因必填） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      header="作废收款退款单"
      theme="warning"
      width="480px"
      :confirm-btn="{ content: '确认作废', loading: cancelSubmitting, theme: 'danger' }"
      :close-on-overlay-click="!cancelSubmitting"
      data-testid="rf-cancel-dialog"
      @confirm="submitCancel"
    >
      <div v-if="cancelTarget" class="dialog-body">
        <div class="dialog-meta">
          <span class="mono">{{ cancelTarget.refund_no }}</span>
          <span class="muted">退款 {{ formatMoney(cancelTarget.amount) }}</span>
        </div>
        <t-form label-align="top" class="form">
          <t-form-item label="作废原因" required-mark>
            <t-textarea
              v-model="cancelReason"
              :maxlength="REFUND_REASON_MAX"
              :autosize="{ minRows: 3, maxRows: 6 }"
              placeholder="例如：客户取消退款 / 金额登记错误，重新登记"
              data-testid="rf-cancel-reason"
            />
          </t-form-item>
        </t-form>
        <p class="form-tip">
          只有「草稿 + 一分钱都没冲减」的退款能作废；作废是不可逆终态
          （状态图里没有 <span class="mono">CANCELLED→*</span> 这条边）。
        </p>
        <p class="alloc__error" data-testid="rf-cancel-error">{{ cancelErrorText }}</p>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 收款退款单页（NEAR-FIX-1 / 卡 t_67dcb9cb）。
 *
 * # 为什么这个页面必须存在
 *
 * 后端 `receipt_refund.rs`（5 个接口 + 审批桥 + 过账）早在 SO-14 就完整了，
 * 但 V0295 建菜单时把 `path` / `component` 留空（那时还没有前端页），
 * 之后那张前端卡一直没来 —— 侧栏里点不到、深链也进不去，等于功能对用户不存在。
 * 本页把这条链补上：列表 / 详情 / 登记 / 冲减 / 确认 / 提交审批 / 撤回 / 作废 / 过账。
 *
 * # 口径来源（逐字抄后端，不猜）
 *
 * | 语义 | 出处 |
 * |------|------|
 * | 列表 / 详情行 | `receipt_refund::RefundRes`（`@/api/business/erp/receipt-refund`） |
 * | 分页入参 | `RefundPageQuery`（`refund_no` 模糊匹配单号 + 客户名） |
 * | 状态域 4 值 | `STATUSES` = DRAFT / CONFIRMED / POSTED / CANCELLED |
 * | 合法边 3 条 | `state_machine::RECEIPT_REFUND_EDGES` |
 * | 冲减 | `POST /{id}/allocate`：单张 ≤ 该应收可退，合计 ≤ 退款金额 |
 * | 过账 | `PATCH /{id}/status` → POSTED，**要求核销满**，否则 409 |
 * | 审批 | `POST /{id}/submit-approval`，通过后自动过账；撤回走流程引擎 |
 * | 原因 | `REASON_MIN_CHARS = 4`，建单与作废都必填 |
 *
 * # 三个容易写错的地方（判据都在 `./receipt-refund-logic.ts`，有单测）
 *
 * 1. **审批不是状态**：`status` 里没有「审批中」。判「已提交待批」只能看
 *    `approval_status === 'PENDING'`；用 status 判会让撤回按钮永远不出现。
 * 2. **过账要求核销满**：`allocated_amount === amount`，否则后端 409
 *    （未冲减的部分借记 1122 就是凭空多记一笔应收）。
 * 3. **没有反核销**：`allocate` 只 UPSERT 不删行，所以「取消勾选」的行会保留原金额 ——
 *    本页的处理是「勾掉即视为 0」，保存时不发该行；已冲减的既有行重新打开时预填原值。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import {
  allocateReceiptRefund,
  changeReceiptRefundStatus,
  createReceiptRefund,
  fetchReceiptRefundDetail,
  fetchReceiptRefundPage,
  submitReceiptRefundApproval,
  type ReceiptRefund,
  type ReceiptRefundDetail,
  type RefundMethod,
} from '@/api/business/erp/receipt-refund';
import {
  fetchCustomerPage,
  fetchReceiptDetail,
  fetchReceivablePage,
  fetchReceiptPage,
  type Customer,
  type Receipt,
} from '@/api/business/erp';
import { getFlowInstance, withdrawFlowInstance } from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import { useUserStore } from '@/stores/user';

import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import {
  hasApprovalProgress,
  loadApprovalProgress,
  EMPTY_APPROVAL_PROGRESS,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';

import { dash, formatCents, formatMoney, shortId, toCents, todayString } from '../shared/money';
import {
  RECEIVABLE_FETCH_LIMIT,
  REFUND_COLUMNS,
  REFUND_METHOD_OPTIONS,
  REFUND_PAGE_SIZE,
  REFUND_PERMS,
  REFUND_REASON_MAX,
  REFUND_STATUS_OPTIONS,
  approvalLabel,
  approvalTheme,
  buildAllocationPayload,
  buildAllocationRows,
  buildRefundQuery,
  canCancel,
  canConfirm,
  canPost,
  canSubmitApproval,
  canWithdraw,
  cancelBlockedReason,
  emptyRefundFilter,
  hasApprovalInstance,
  isAllocatable,
  isRefundFiltered,
  postBlockedReason,
  receiptRefundableCap,
  refundStatusTheme,
  refundableCents,
  submitBlockedReason,
  unallocatedCents,
  validateAllocations,
  validateRefundAmount,
  validateRefundReason,
  type AllocationRow,
  type RefundFilter,
} from './receipt-refund-logic';

// ==================== 基础状态 ====================

const userStore = useUserStore();
const hasCreatePermission = computed(() => userStore.hasPermission(REFUND_PERMS.create));
const hasAllocatePermission = computed(() => userStore.hasPermission(REFUND_PERMS.allocate));
const hasStatusPermission = computed(() => userStore.hasPermission(REFUND_PERMS.status));

const filter = reactive<RefundFilter>(emptyRefundFilter());
const statusOptions = REFUND_STATUS_OPTIONS.map((item) => ({ label: item.label, value: item.value }));
const methodOptions = REFUND_METHOD_OPTIONS.map((item) => ({ label: item.label, value: item.value }));

const rows = ref<ReceiptRefund[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = REFUND_PAGE_SIZE;
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

const createTip = computed(() =>
  hasCreatePermission.value ? '' : `缺少权限 ${REFUND_PERMS.create}`,
);

function messageOf(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return error instanceof Error ? error.message : String(error);
}

/** 状态按钮的置灰原因（权限优先，其次业务判据） */
function statusTip(_row: ReceiptRefund, allowed: boolean, reason: string): string {
  if (!hasStatusPermission.value) return `缺少权限 ${REFUND_PERMS.status}`;
  if (!allowed) return reason;
  return '';
}

// ==================== 列表 ====================

async function loadRefunds(page = 1): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchReceiptRefundPage(buildRefundQuery(filter, page, pageSize));
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
  customers.value = await fetchCustomerPage({ page: 1, page_size: 200 })
    .then((data) => data.list ?? [])
    .catch(() => [] as Customer[]);
}

function handleSearch(): void {
  void loadRefunds(1);
}

function handleReset(): void {
  Object.assign(filter, emptyRefundFilter());
  void loadRefunds(1);
}

function handlePageChange(pageInfo: { current: number }): void {
  void loadRefunds(pageInfo.current);
}

function refresh(): void {
  void loadRefunds(current.value);
}

onMounted(async () => {
  await Promise.all([loadCustomers(), loadRefunds(1)]);
});

// ==================== 登记退款 ====================

const createVisible = ref(false);
const createSubmitting = ref(false);
const sourceReceiptLoading = ref(false);
const sourceReceipts = ref<Receipt[]>([]);
/** 来源收款单的可退上限（分）；null = 还不知道 / 未选来源收款单 */
const capCents = ref<number | null>(null);
const capHint = ref('');

const createForm = reactive<{
  customerId: string;
  sourceReceiptId: string;
  amount: number;
  refundDate: string;
  method: RefundMethod | '';
  bankAccount: string;
  reason: string;
  remark: string;
}>({
  customerId: '',
  sourceReceiptId: '',
  amount: 0,
  refundDate: todayString(),
  method: '',
  bankAccount: '',
  reason: '',
  remark: '',
});

const sourceReceiptOptions = computed(() =>
  sourceReceipts.value.map((item) => ({
    label: `${item.receipt_no}（${formatMoney(item.amount)} · ${item.status_label || item.status}）`,
    value: item.id,
  })),
);

function openCreate(): void {
  if (!hasCreatePermission.value) {
    MessagePlugin.warning(createTip.value);
    return;
  }
  Object.assign(createForm, {
    customerId: '',
    sourceReceiptId: '',
    amount: 0,
    refundDate: todayString(),
    method: '',
    bankAccount: '',
    reason: '',
    remark: '',
  });
  sourceReceipts.value = [];
  capCents.value = null;
  capHint.value = '';
  createVisible.value = true;
}

/** 换客户：清空来源收款单与上限，重新拉该客户的收款单 */
async function handleCreateCustomerChange(): Promise<void> {
  createForm.sourceReceiptId = '';
  capCents.value = null;
  capHint.value = '';
  sourceReceipts.value = [];
  if (!createForm.customerId) {
    return;
  }
  sourceReceiptLoading.value = true;
  try {
    const data = await fetchReceiptPage({
      page: 1,
      page_size: 200,
      customer_id: createForm.customerId,
    });
    // 只留「已经核销过、可能有钱可退」的收款单（草稿收款一分没进账，退不了）
    sourceReceipts.value = (data.list ?? []).filter(
      (item) => item.status === 'CONFIRMED' || item.status === 'POSTED',
    );
  } catch {
    sourceReceipts.value = [];
  } finally {
    sourceReceiptLoading.value = false;
  }
}

/**
 * 选来源收款单 → 带出可退金额。
 *
 * 算法：该收款单核销过的每一张应收，取其当前 `received_amount` 之和（`receiptRefundableCap`）。
 * 这是**粗上限**：真正的可退金额在冲减那一步逐张算（多笔收款可能冲到同一张应收）。
 */
async function handleCreateSourceChange(): Promise<void> {
  capCents.value = null;
  capHint.value = '';
  if (!createForm.sourceReceiptId) {
    return;
  }
  sourceReceiptLoading.value = true;
  try {
    const receipt = await fetchReceiptDetail(createForm.sourceReceiptId);
    const allocations = receipt.allocations ?? [];
    if (allocations.length === 0) {
      capHint.value = '该收款单还没有核销到应收（一分钱没冲账），没有可退的钱 —— 请先到「收款单」核销。';
      capCents.value = 0;
      return;
    }
    const receivedCents = new Map<string, number>();
    // 统一拉该客户的应收（含 received_amount），再按 id 取 —— 逐张查是 N+1
    const page = await fetchReceivablePage({
      page: 1,
      page_size: RECEIVABLE_FETCH_LIMIT,
      customer_id: receipt.customer_id,
    });
    for (const ar of page.list ?? []) {
      receivedCents.set(ar.id, toCents(ar.received_amount));
    }
    capCents.value = receiptRefundableCap(allocations, receivedCents);
    const receivableCount = new Set(allocations.map((item) => item.receivable_id)).size;
    capHint.value = `该收款单冲减过 ${receivableCount} 张应收，当前可退上限 ${formatCents(capCents.value)}（逐张可退之和可能更小）`;
    if (createForm.amount === 0 && capCents.value > 0) {
      createForm.amount = capCents.value / 100;
    }
  } catch (error) {
    capHint.value = `可退金额查询失败：${messageOf(error)}（不影响登记，冲减时后端会再校验）`;
  } finally {
    sourceReceiptLoading.value = false;
  }
}

async function submitCreate(): Promise<void> {
  if (!createForm.customerId) {
    MessagePlugin.warning('请选择客户');
    return;
  }
  const amountCents = Math.round(Math.abs(createForm.amount) * 100);
  const amountError = validateRefundAmount(amountCents, capCents.value);
  if (amountError) {
    MessagePlugin.warning(amountError);
    return;
  }
  const reasonError = validateRefundReason(createForm.reason);
  if (reasonError) {
    MessagePlugin.warning(reasonError);
    return;
  }
  createSubmitting.value = true;
  try {
    const created = await createReceiptRefund({
      customer_id: createForm.customerId,
      source_receipt_id: createForm.sourceReceiptId || null,
      refund_date: createForm.refundDate || null,
      amount: amountCents / 100,
      method: createForm.method || null,
      bank_account: createForm.bankAccount || null,
      reason: createForm.reason.trim(),
      remark: createForm.remark || null,
    });
    MessagePlugin.success(`已登记退款单 ${created.refund_no}（草稿）：下一步在「核销」里冲减应收的已收金额`);
    createVisible.value = false;
    await loadRefunds(1);
    await openDetail(created);
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
const detail = ref<ReceiptRefundDetail | null>(null);
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });

async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  detailError.value = '';
  try {
    const data = await fetchReceiptRefundDetail(id);
    detail.value = data;
    // 审批进度：没有实例 id 就什么都不显示（草稿 / 从未送审别画空标题）
    approvalProgress.value = await loadApprovalProgress(
      data.approval?.instance_id || data.approval_instance_id,
      getFlowInstance,
    );
  } catch (error) {
    detail.value = null;
    approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

async function openDetail(row: Pick<ReceiptRefund, 'id'>): Promise<void> {
  detailVisible.value = true;
  await loadDetail(row.id);
}

function reloadDetail(): void {
  if (detail.value) {
    void loadDetail(detail.value.id);
  }
}

// ==================== 状态操作 ====================

async function applyStatus(
  row: Pick<ReceiptRefund, 'id' | 'refund_no'>,
  status: 'CONFIRMED' | 'POSTED',
  reason: string | null,
  successText: string,
): Promise<void> {
  if (!hasStatusPermission.value) {
    MessagePlugin.warning(`缺少权限 ${REFUND_PERMS.status}`);
    return;
  }
  try {
    const updated = await changeReceiptRefundStatus(row.id, { status, reason });
    MessagePlugin.success(`${successText}：${updated.refund_no}`);
    await afterMutation(updated);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  }
}

async function confirmRefund(row: Pick<ReceiptRefund, 'id' | 'refund_no'>): Promise<void> {
  await applyStatus(row, 'CONFIRMED', null, '已确认（下一步：核销 → 提交审批）');
}

async function postRefund(row: Pick<ReceiptRefund, 'id' | 'refund_no'>): Promise<void> {
  await applyStatus(
    row,
    'POSTED',
    null,
    '已过账：生成退款凭证（借 1122 应收 / 贷 1002 银行存款），应收已恢复',
  );
}

async function submitApproval(row: Pick<ReceiptRefund, 'id' | 'refund_no'>): Promise<void> {
  if (!hasStatusPermission.value) {
    MessagePlugin.warning(`缺少权限 ${REFUND_PERMS.status}`);
    return;
  }
  try {
    const updated = await submitReceiptRefundApproval(row.id, {});
    // 审批不是状态：文案必须说清「账还没动」
    MessagePlugin.info(
      `已提交审批：${updated.refund_no}（审批通过后才过账，当前仍是「${updated.status_label}」）`,
    );
    await afterMutation(updated);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  }
}

const withdrawingId = ref('');

async function withdrawApproval(row: Pick<ReceiptRefund, 'id' | 'refund_no' | 'approval_instance_id'>): Promise<void> {
  const instanceId = row.approval_instance_id;
  if (!instanceId) {
    MessagePlugin.warning('这张单没有可撤回的审批实例');
    return;
  }
  withdrawingId.value = row.id;
  try {
    await withdrawFlowInstance(instanceId, '页面撤回');
    MessagePlugin.success(`已撤回审批：${row.refund_no} 仍是「已确认」（账一分没动，可改完再提）`);
    await afterMutation(row);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    withdrawingId.value = '';
  }
}

/** 写操作之后的统一收尾：刷新详情 + 列表 */
async function afterMutation(row: { id: string }): Promise<void> {
  if (detail.value?.id === row.id) {
    await loadDetail(row.id);
  }
  await loadRefunds(current.value);
}

// ==================== 作废 ====================

const cancelVisible = ref(false);
const cancelSubmitting = ref(false);
const cancelTarget = ref<Pick<ReceiptRefund, 'id' | 'refund_no' | 'amount'> | null>(null);
const cancelReason = ref('');
const cancelErrorText = ref('');

function openCancel(row: Pick<ReceiptRefund, 'id' | 'refund_no' | 'amount'>): void {
  if (!hasStatusPermission.value) {
    MessagePlugin.warning(`缺少权限 ${REFUND_PERMS.status}`);
    return;
  }
  cancelTarget.value = row;
  cancelReason.value = '';
  cancelErrorText.value = '';
  cancelVisible.value = true;
}

async function submitCancel(): Promise<void> {
  const target = cancelTarget.value;
  if (!target) {
    return;
  }
  const reasonError = validateRefundReason(cancelReason.value);
  if (reasonError) {
    cancelErrorText.value = reasonError;
    return;
  }
  cancelSubmitting.value = true;
  cancelErrorText.value = '';
  try {
    const updated = await changeReceiptRefundStatus(target.id, {
      status: 'CANCELLED',
      reason: cancelReason.value.trim(),
    });
    MessagePlugin.success(`已作废：${updated.refund_no}`);
    cancelVisible.value = false;
    await afterMutation(updated);
  } catch (error) {
    cancelErrorText.value = messageOf(error);
  } finally {
    cancelSubmitting.value = false;
  }
}

// ==================== 核销（冲减应收回写） ====================

const allocVisible = ref(false);
const allocLoading = ref(false);
const allocSubmitting = ref(false);
const allocTarget = ref<ReceiptRefundDetail | null>(null);
const allocRows = ref<AllocationRow[]>([]);

const allocCheck = computed(() =>
  validateAllocations(allocRows.value, toCents(allocTarget.value?.amount)),
);
const allocTotalCents = computed(() => allocCheck.value.totalCents);
/** 只有用户填了内容才提示错误（一打开就满屏红字没意义） */
const allocValidation = computed(() =>
  allocCheck.value.filledCount > 0 || allocRows.value.some((row) => row.inputAmount !== null)
    ? allocCheck.value.error
    : null,
);

async function openAllocation(row: Pick<ReceiptRefund, 'id'>): Promise<void> {
  if (!hasAllocatePermission.value) {
    MessagePlugin.warning(`缺少权限 ${REFUND_PERMS.allocate}`);
    return;
  }
  allocVisible.value = true;
  allocLoading.value = true;
  allocRows.value = [];
  allocTarget.value = null;
  try {
    const target = detail.value?.id === row.id ? detail.value : await fetchReceiptRefundDetail(row.id);
    allocTarget.value = target;
    const page = await fetchReceivablePage({
      page: 1,
      page_size: RECEIVABLE_FETCH_LIMIT,
      customer_id: target.customer_id,
    });
    allocRows.value = buildAllocationRows(page.list ?? [], target.allocations ?? []);
    if (allocRows.value.length === 0) {
      MessagePlugin.warning('该客户没有可冲减的应收（收款退款只能冲已经收到的钱）');
    }
  } catch (error) {
    MessagePlugin.error(messageOf(error));
    allocVisible.value = false;
  } finally {
    allocLoading.value = false;
  }
}

function toggleAllocRow(row: AllocationRow, checked: boolean): void {
  row.inputAmount = checked ? (refundableCents(row.receivedCents, row.alreadyCents) / 100) : null;
}

function setAllocAmount(row: AllocationRow, value: unknown): void {
  const num = typeof value === 'number' ? value : Number(value);
  row.inputAmount = Number.isFinite(num) ? num : null;
}

/** 按「先冲最早的账」把退款金额依次填满 */
function fillAll(): void {
  const target = allocTarget.value;
  if (!target) {
    return;
  }
  let left = toCents(target.amount);
  for (const row of allocRows.value) {
    if (left <= 0) {
      row.inputAmount = null;
      continue;
    }
    const cap = refundableCents(row.receivedCents, row.alreadyCents);
    const take = Math.min(cap, left);
    row.inputAmount = take > 0 ? take / 100 : null;
    left -= take;
  }
}

async function submitAllocation(): Promise<void> {
  const target = allocTarget.value;
  if (!target) {
    return;
  }
  const check = validateAllocations(allocRows.value, toCents(target.amount));
  if (check.error) {
    MessagePlugin.warning(check.error);
    return;
  }
  allocSubmitting.value = true;
  try {
    const updated = await allocateReceiptRefund(target.id, buildAllocationPayload(allocRows.value));
    MessagePlugin.success(`已冲减应收 ${formatCents(check.totalCents)}（应收已收金额已回退）`);
    allocVisible.value = false;
    await afterMutation(updated);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    allocSubmitting.value = false;
  }
}
</script>

<style scoped>
.rf-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-card,
.list-card {
  background: var(--td-bg-color-container);
  border-radius: var(--td-radius-medium);
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.tool-row--second {
  margin-top: 10px;
}

.tool-row__spacer {
  flex: 1 1 auto;
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
  width: 190px;
}

.tool-row__method {
  width: 120px;
}

.tool-row__no {
  width: 220px;
}

.tool-row__date {
  width: 140px;
}

.op-wrap {
  display: inline-block;
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
  gap: 8px;
}

.panel-head__title {
  font-size: 15px;
  font-weight: 600;
}

.panel-head__meta,
.panel-head__tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.list-alert {
  margin-bottom: 10px;
}

.dialog-body {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.dialog-meta {
  display: flex;
  align-items: baseline;
  gap: 10px;
  font-size: 13px;
}

.form {
  margin-top: 4px;
}

.form-tip {
  margin: 4px 0 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.detail-head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}

.detail-head__no {
  font-size: 16px;
  font-weight: 600;
}

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px 18px;
  margin-bottom: 16px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.field--wide {
  grid-column: span 2;
}

.field__label {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.field__value {
  font-size: 13px;
  word-break: break-all;
}

.section-title {
  margin: 14px 0 6px;
  font-size: 14px;
  font-weight: 600;
}

.section-tip {
  margin: 0 0 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.mini-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12px;
}

.mini-table th,
.mini-table td {
  padding: 6px 8px;
  border-bottom: 1px solid var(--td-component-stroke);
  text-align: left;
}

.mini-table__num {
  text-align: right;
}

.mini-table__check {
  width: 56px;
  text-align: center;
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

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.empty {
  padding: 14px 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.alloc__head,
.alloc__summary,
.alloc__actions {
  display: flex;
  align-items: center;
  gap: 10px;
}

.alloc__head {
  justify-content: space-between;
  margin-bottom: 8px;
  font-size: 13px;
}

.alloc__summary {
  margin: 10px 0 0;
  font-size: 13px;
}

.alloc__error {
  min-height: 18px;
  margin: 4px 0 0;
  color: var(--td-error-color);
  font-size: 12px;
}

.alloc__actions {
  margin-top: 12px;
  justify-content: flex-end;
}

.detail-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  margin-top: 18px;
  padding-top: 14px;
  border-top: 1px solid var(--td-component-stroke);
}
</style>
