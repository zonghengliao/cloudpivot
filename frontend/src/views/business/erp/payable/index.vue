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
            <!-- ★ P-FLOW-3B：作废审批中 → 「作废」消失，换「撤回作废」（撤回后单据回「未付」） -->
            <t-link
              v-if="canWithdrawCancel(row, hasWithdrawPermission)"
              theme="warning"
              :data-testid="`btn-payable-withdraw-${row.payable_no}`"
              @click="openWithdrawCancel(row)"
            >
              撤回作废
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
          <!--
            R-P1-5：关键动作吸顶（用户原话「不能满屏去找按钮」）。
            付款 / 作废 / 撤回作废原来埋在正文中段（付款）与列表行（作废），点进详情就找不到。
          -->
          <DocActionBar title="关键操作" testid="payable-action-bar">
            <t-button
              v-if="canStartPay(payInfo?.payable, hasPayPermission, payInfo?.payable.status ?? detail.status)"
              theme="primary"
              data-testid="btn-payable-drawer-pay"
              @click="openPayFromDetail"
            >
              发起付款（未付 {{ formatMoney(payPrefillAmount(payInfo?.payable)) }}）
            </t-button>
            <span
              v-else-if="isPendingCancel(detail)"
              class="drawer-actions__blocked"
              data-testid="payable-pay-blocked-pending"
            >
              作废审批中不能付款（撤回或驳回后回到「未付」才能付）
            </span>
            <!--
              ★ P3-07：审批已通过但还没执行 —— 后端的 `blocked_reason` 说的是
              「已有一笔付款在审批中，请等待审批结果」，审批其实已经结束了，照抄会误导。
            -->
            <span
              v-else-if="awaitingExecutePayment"
              class="drawer-actions__blocked"
              data-testid="payable-pay-blocked-awaiting-executor"
            >
              付款 {{ awaitingExecutePayment.payment_no }} 审批已通过、钱还没付出去：
              请让另一位有付款权限的账号点下方付款记录里的「执行付款」
            </span>
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

            <t-button
              v-if="canCancel(detail)"
              theme="danger"
              variant="outline"
              data-testid="btn-payable-drawer-cancel"
              @click="openCancel(detail)"
            >
              <template #icon><t-icon name="delete" /></template>
              作废
            </t-button>
            <t-button
              v-if="isPendingCancel(detail) && canWithdrawCancel(detail, hasWithdrawPermission)"
              theme="warning"
              variant="outline"
              data-testid="btn-payable-bar-withdraw"
              @click="openWithdrawCancel(detail)"
            >
              撤回作废审批
            </t-button>
          </DocActionBar>
          <!-- ★ P-FLOW-3B：作废审批中（欠款一分没动）→ 一句话说清 + 撤回入口 -->
          <t-alert
            v-if="isPendingCancel(detail)"
            theme="warning"
            message="作废审批中：欠款一分没动，等财务经理复核"
            :description="pendingCancelDescription(detail)"
            data-testid="payable-pending-cancel"
          >
            <template #operation>
              <t-button
                v-if="canWithdrawCancel(detail, hasWithdrawPermission)"
                size="small"
                variant="outline"
                theme="warning"
                data-testid="btn-payable-drawer-withdraw"
                @click="openWithdrawCancel(detail)"
              >
                撤回作废审批
              </t-button>
            </template>
          </t-alert>

          <!--
            ★ P-FLOW-3B 补漏：作废原因直接读 `PayableRes::cancel_reason`（列表 + 详情都下发），
            不是「送审那一刻」的临时状态 —— 刷新 / 重开抽屉后仍在。
          -->
          <p
            v-if="isPendingCancel(detail) && cancelReasonText(detail)"
            class="drawer-cancel-reason"
            data-testid="payable-pending-cancel-reason"
          >
            <span class="muted">作废原因：</span>{{ cancelReasonText(detail) }}
          </p>

          <!-- R-P1-5：「发起付款」已上移到吸顶动作条（见顶部 DocActionBar），这里不再重复 -->
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
                  <!--
                    ★ P3-07：审批通过但**还没执行**（`erp.separation_of_duties.approve_execute`
                    打开时的两段式付款）→ 给另一位有付款权限的人一个执行入口。
                    开关关闭时审批通过即自动记账（付款直接是「已通过」），按钮不会出现。

                    ATT-09：出款前必须已有银行回单（规则 `PAYMENT` + `ACTIVATE`，后端
                    `payable_payment::execute_...` 同一条闸门）→ 缺回单时按钮置灰 + 红字。
                  -->
                  <template v-if="isAwaitingExecutor(item, instanceStatusOf(item))">
                    <t-tooltip :content="payGateTip(item)" :disabled="!payGateTip(item)" placement="top-left">
                      <span class="op-wrap">
                        <t-button
                          v-privilege="[PAY_PERMISSION]"
                          class="payment-row__execute"
                          size="small"
                          theme="primary"
                          :loading="executingPaymentId === item.id"
                          :disabled="!hasPayPermission || !payGateOf(item).canProceed"
                          :data-testid="`payable-payment-execute-${item.payment_no}`"
                          @click="openExecutePayment(item)"
                        >
                          执行付款
                        </t-button>
                      </span>
                    </t-tooltip>
                    <AttachmentGateHint
                      :hint="payGateOf(item).hint"
                      :testid="`payable-payment-gate-hint-${item.payment_no}`"
                    />
                  </template>
                </div>
                <div v-if="item.remark" class="payment-row__remark">{{ item.remark }}</div>
              </t-timeline-item>
            </t-timeline>
            <p v-else class="payments__empty" data-testid="payable-payments-empty">
              还没有付款记录（未付 {{ formatMoney(unpaidOf(detail)) }}）
            </p>
          </div>

          <!--
            ★ P17：付款审批进度（复用审批中心的 FlowProgress，进度只来自引擎写的节点行）。
            C2：`tasks` 也传（同一接口的 `timeline[]`）—— 付款 / 作废审批都能看到
            谁审的、什么时间、意见或跳过说明，与采购 / 收货 / 凭证 / 调拨统一。
          -->
          <template v-if="hasApprovalProgress(approvalProgress)">
            <div class="section-title">{{ approvalTitle }}</div>
            <FlowProgress
              :nodes="approvalProgress.nodes"
              :tasks="approvalProgress.tasks"
              data-testid="payable-approval-progress"
            />
          </template>

          <p v-if="showActionNote" class="drawer-note">
            详情里可直接「发起付款」（金额预填<strong>未付</strong>，不是应付总额）。配了「应付付款审批」流程时，付款只是
            <strong>发起审批</strong>（账不动，审批通过才生成「借 2202 应付账款 / 贷 1002 银行存款」的凭证）；
            没配流程则立即生效。付款记录里的「应付单号 / 凭证」都能点回去。
            作废同理：配了「应付作废审批」流程时先送审（单据变<strong>作废审批中</strong>、欠款不动），
            财务经理通过才真正作废并红冲进项凭证。
          </p>

          <!--
            附件：发票影像 / 付款回单（ARCH-3 DC-17）—— 四个单据详情页共用同一组件。
            挂的是**应付单**（`PAYABLE`）：这是页面上的单据本体；单笔付款登记行
            （`biz_payment`）另有 `PAYMENT` 类型可用，接口一视同仁
            （`GET /api/business/attachment/list?doc_type=PAYMENT&doc_id=<付款行 id>`）。
          -->
          <div class="section-title">附件</div>
          <DocAttachmentPanel
            :doc-type="DOC_KIND.PAYABLE"
            :doc-id="detail.id"
            default-role="INVOICE_SCAN"
            testid-prefix="payable-attach"
          />

          <!--
            ATT-09：单笔付款的银行回单（`PAYMENT` + `ACTIVATE`）—— 出款前必传。
            挂的是**付款行**而不是应付单：闸门查的就是 `biz_payment` 这一行缺不缺回单
            （`payable_payment.rs` 的 `ensure_attachments(Payment, payment_id, Activate)`）。
            多笔付款时用下拉选哪一笔；被「执行付款」拦住时会自动选中那一笔。
          -->
          <template v-if="(detail.payments?.length ?? 0) > 0">
            <div class="section-title">付款回单（出款前必传）</div>
            <t-select
              v-if="(detail.payments?.length ?? 0) > 1"
              v-model="payAttachDocId"
              size="small"
              class="pay-attach-select"
              :options="payAttachOptions"
              data-testid="payable-pay-attach-select"
            />
            <p class="muted pay-attach-tip" data-testid="payable-pay-attach-tip">
              选中的付款：{{ selectedPaymentLabel }}
            </p>
            <DocAttachmentPanel
              :doc-type="DOC_KIND.PAYMENT"
              :doc-id="payAttachDocId"
              default-role="BANK_RECEIPT"
              testid-prefix="payment-attach"
              @changed="refreshPaymentGates()"
            />
          </template>
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
        <!--
          ATT-09 / N-5（验收④「新建态不调接口，但有提示」）：付款记录在这一步还没有 id
          （提交动作内部才创建），所以查不了 `/attachment/check`；但规则确定
          （`PAYMENT` + `ACTIVATE`，V0347 / 145 实测：银行回单 pdf/jpg/png 至少 1 个）。
          配了「应付付款审批」流程时：审批通过后在详情「付款回单」面板上传，缺回单时
          「执行付款」按钮置灰 + 红字；没配流程则提交即生效（回单事后再补传）。
        -->
        <t-alert
          theme="warning"
          data-testid="payable-pay-attach-notice"
          message="出款前必须上传银行回单（pdf/jpg/png，至少 1 个）：审批通过后在详情「付款回单」面板上传，缺回单时「执行付款」按钮会置灰；未配付款审批流程时提交即生效，回单请事后补传。"
        />
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

    <!-- ==================== ⑤ 作废弹窗（原因必填 → P-FLOW-3B 起走审批） ==================== -->
    <t-dialog
      v-model:visible="cancelVisible"
      header="提交应付单作废"
      theme="warning"
      width="480px"
      :confirm-btn="{ content: '提交作废', loading: cancelSubmitting, theme: 'danger' }"
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
          配了「应付作废审批」流程时，提交后单据变<strong>作废审批中</strong>（欠款不动、不能再付款），
          财务经理复核通过才真正作废并红冲这张应付单的进项凭证；没配流程则立即生效。
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
import { exportFilterPayload } from '@/utils/batch-action';
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
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
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
  submitCancelPayable,
  fetchPayableDetail,
  executePayment,
  fetchPayablePaymentInfo,
  payPayable,
  withdrawCancelPayable,
  type PayableDetail,
  type PayablePayment,
  type PayablePaymentInfo,
} from './payable-write-api';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import AttachmentGateHint from '@/views/business/erp/shared/AttachmentGateHint.vue';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import DocActionBar from '@/views/business/erp/shared/DocActionBar.vue';
import {
  degradedGate,
  gateButtonTip,
  gateFromCheck,
  pendingGate,
  type AttachmentGate,
} from '@/views/business/erp/shared/attachment-gate-pure';
import { checkDocAttachments } from '@/api/business/erp/attachment';
import { getFlowInstance } from '@/api/business/flow-api';
import {
  EMPTY_APPROVAL_PROGRESS,
  hasApprovalProgress,
  loadApprovalProgress,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
import {
  PAYABLE_PAGE_SIZE,
  PAYABLE_STATUS_FILTER_OPTIONS,
  approvalTargetOf,
  buildPayableQuery,
  canCancelPayable as canCancel,
  canPayPayable as canPay,
  canStartPay,
  canWithdrawCancel,
  cancelReasonText,
  emptyPayableFilter,
  executePaymentConfirmText,
  executorProbeInstanceId,
  isAwaitingExecutor,
  isPayableFiltered,
  isPendingCancel,
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
  withdrawCancelBlockedReason,
  type PayableFilterState,
} from './payable-logic';
import { submitFeedback, withdrawFeedback } from '../shared/approval-flow';
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
  { colKey: 'status', title: '状态', width: 110 },
  // P-FLOW-3B：作废审批中那行会多一个「撤回作废」入口，操作列要给得下
  { colKey: 'op', title: '操作', width: 130, fixed: 'right' },
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
  // 应付单没有「关键词」字段（列表按单号 / 供应商 / 到期日筛），导出沿用单号筛选口径；
  // 卡 t_f83ae18f：状态筛选同样透传 —— ERP 单据的状态是**文本码**，走 `status_code`
  // （传整型 `status` 会被后端 422+10001 拒掉，「导出当前筛选结果」会静默丢掉状态）。
  return payableBatchExport({
    ids,
    ...exportFilterPayload({ keyword: filter.payable_no, statusCode: filter.status }),
  });
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
    // P-FLOW-3B：作废审批中用 `cancel_instance_id`（新字段）；否则看有没有审批中的付款
    // （`approvalTargetOf` 是纯函数，单测直接打；失败不影响详情展示）
    await refreshApprovalProgress(detail.value);
    // ★ P3-07：付款「审批通过但还没执行」的判据（后端没下发 `awaiting_executor` 时探一次实例状态）
    await loadExecutorGate(detail.value);
    // ★ ATT-09：待执行付款的银行回单闸门（缺回单 → 「执行付款」置灰 + 红字）
    syncPayAttachSelection();
    await refreshPaymentGates();
  } catch (error) {
    void MessagePlugin.error(messageOf(error));
  } finally {
    detailLoading.value = false;
  }
  // ★ D5：可付信息（能不能付 / 默认金额 / 超付提示）—— 单独一个只读接口，失败不挡详情
  await loadPayInfo(row.id);
}

/**
 * 按单据当前状态选要展示的审批进度（进度只来自引擎写的节点行）。
 *
 * 作废审批中 → 「作废审批进度」（实例 id 来自 `PayableRes::cancel_instance_id`，列表 + 详情都下发，
 * 所以**重新打开抽屉后仍在**）；否则 → 那笔审批中的付款。
 */
async function refreshApprovalProgress(row: PayableDetail): Promise<void> {
  const pendingPay = row.payments?.find((item) => item.approval_instance_id);
  const target = approvalTargetOf(row, pendingPay?.approval_instance_id);
  await loadApprovalProgressOf(target.instanceId, target.title);
}

/**
 * 重新拉详情（付款 / 作废提交成功后的唯一刷新入口）。
 *
 * # 为什么不能只信写接口回包
 *
 * `PaySubmitRes.detail` 是**那次写事务提交后**的快照。页面拿它当唯一数据源时，只要后端把
 * 详情拼在提交前（P2-2B 手册真机采集就撞到过：提交成功、弹窗已关，抽屉里仍是「付款记录
 * 0 笔 / 未付 80.00」，关掉抽屉重开才变成「共 1 笔 30.00」），抽屉就会一直显示旧账，
 * 用户会以为付款没登记上。读接口才是权威口径：付款记录、已付 / 未付、单据状态、审批进度
 * 一次重取。
 *
 * @param id 应付单 id
 * @param approvalInstanceId 刚发起的审批实例（详情里查不到「审批中的付款」时兜底用之）
 */
async function refreshDetail(id: string, approvalInstanceId?: string | null): Promise<void> {
  try {
    const fresh = await fetchPayableDetail(id);
    detail.value = fresh;
    await refreshApprovalProgress(fresh);
    if (approvalInstanceId && !hasApprovalProgress(approvalProgress.value)) {
      await loadApprovalProgressOf(approvalInstanceId);
    }
    // ★ P3-07：审批进度变了 → 「执行付款」按钮的判据跟着重算
    await loadExecutorGate(fresh);
    // ★ ATT-09：待执行付款的银行回单闸门一起重算（刚传完回单 → 按钮自动变亮）
    syncPayAttachSelection();
    await refreshPaymentGates();
  } catch (error) {
    // 抽屉不关：单头 / 付款记录仍是上一次的快照，但至少要说清「没刷新成功」
    void MessagePlugin.warning(`详情刷新失败：${messageOf(error)}`);
  }
  // ★ D5：可付信息一起重取（未付变了 → 按钮文案 / 能否再付都要重算）
  await loadPayInfo(id);
}

// ==================== D5：可付信息（应付单 → 发起付款） ====================

/** 付款权限码（按钮「显不显示」= 后端 `can_pay` **且** 有本码） */
const PAY_PERMISSION = 'business:erp:payable:pay';

/**
 * 撤回作废审批的权限码（P-FLOW-3B）。
 *
 * ⚠️ 与 `PAY_PERMISSION` 不同，这个码 V0191 只授给了 `SUPER_ADMIN`（老作废码
 * `business:erp:payable:cancel` 才是 FINANCE / ACCOUNTANT 都有的）。没码的人看到
 * 「作废审批中」但看不到撤回入口，详情里会说明原因 —— 这里不猜权限，直接查 store。
 * （已开子卡让后端把 4 个新码按老码的角色矩阵补齐。）
 */
const WITHDRAW_CANCEL_PERMISSION = 'business:erp:payable:withdraw-cancel';

const userStore = useUserStore();
const router = useRouter();
const route = useRoute();

/** 当前账号能不能发起付款（按钮 v-if 用；不是置灰） */
const hasPayPermission = computed(() => userStore.hasPermission(PAY_PERMISSION));

/** 当前账号能不能撤回作废审批（决定「撤回作废审批」入口显不显示） */
const hasWithdrawPermission = computed(() => userStore.hasPermission(WITHDRAW_CANCEL_PERMISSION));

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

// ==================== P3-07：审批 ≠ 执行（「执行付款」按钮） ====================

/**
 * 付款审批**实例状态**缓存（`实例 id → APPROVED / RUNNING / …`；`UNKNOWN` = 拉不到）。
 *
 * 只在后端**没有**下发 `awaiting_executor` 时才需要（老后端兜底）：
 * 「审批通过但还没执行」= 付款仍 `PENDING` 且实例已 `APPROVED`，这个状态只有实例详情知道。
 * 审批进度块（`loadApprovalProgressOf`）本来就会拉同一张实例，顺手把状态记进来 ——
 * 于是常态下**零额外请求**。
 */
const paymentInstanceStatus = ref<Record<string, string>>({});

/** 正在执行的那笔付款 id（按钮 loading / 防重复点） */
const executingPaymentId = ref('');

/** 记一条实例状态（`status` 空 = 未知，按钮不出现） */
function cacheInstanceStatus(instanceId: string | null | undefined, status: string | null | undefined): void {
  if (!instanceId) {
    return;
  }
  paymentInstanceStatus.value = { ...paymentInstanceStatus.value, [instanceId]: (status ?? '').trim() };
}

/**
 * 某笔付款的审批实例状态（没实例 / 还没查到 → `null`）。
 *
 * 供 `isAwaitingExecutor(item, instanceStatusOf(item))` 用：模板里直接算，不缓存派生布尔值
 * （避免「实例状态后到、缓存先定死」这类假阴性）。
 */
function instanceStatusOf(item: PayablePayment): string | null {
  const instanceId = (item.approval_instance_id ?? '').trim();
  if (!instanceId) {
    return null;
  }
  return paymentInstanceStatus.value[instanceId] ?? null;
}

/** 详情里那笔「审批通过、等别人执行」的付款（没有 → `null`；动作条提示 / 按钮判据共用） */
const awaitingExecutePayment = computed<PayablePayment | null>(() => {
  const list = detail.value?.payments ?? [];
  return list.find((item) => isAwaitingExecutor(item, instanceStatusOf(item))) ?? null;
});

// ==================== ATT-09：付款回单闸门（PAYMENT / ACTIVATE） ====================

/**
 * 每笔付款的附件闸门（键 = 付款行 id）。
 *
 * # 为什么是「一张表」而不是一个 `useAttachmentGate`
 *
 * 一张应付单可以有**多笔**付款（部分付款），每笔都要出自己的「缺回单」红字。
 * 单实例组合式函数只能盯一个 document id，所以这里按 id 缓存多份闸门 ——
 * 判定 / 文案仍然全部走 `gateFromCheck`（与 `useAttachmentGate` 同一套纯函数，
 * 后端 `GET /attachment/check` 同一条 SQL），不另写第二套判定。
 *
 * 查询失败 → `degradedGate`（**不禁用按钮、不画红字**），与 `use-attachment-gate.ts`
 * 文件头的降级口径一致：真正的闸门在后端（绕过前端直接调照样 400）。
 */
const paymentGates = ref<Record<string, AttachmentGate>>({});

/** 某笔付款的闸门（还没查过 → 初始态：不禁用、不画红字） */
function payGateOf(item: PayablePayment): AttachmentGate {
  return paymentGates.value[item.id] ?? pendingGate();
}

/** 执行付款按钮的 tooltip：权限优先，其次回单缺口（定序在 `gateButtonTip` 里） */
function payGateTip(item: PayablePayment): string {
  return gateButtonTip(payGateOf(item), hasPayPermission.value ? '' : `缺少权限 ${PAY_PERMISSION}`);
}

/**
 * 重查「审批通过待执行」那些付款的回单闸门。
 *
 * 只查待执行的那几笔：其它付款（审批中 / 已付 / 已终止）没有「出款前」这个动作，
 * 查了也只是多几次请求。
 */
async function refreshPaymentGates(): Promise<void> {
  const list = (detail.value?.payments ?? []).filter((item) =>
    isAwaitingExecutor(item, instanceStatusOf(item)),
  );
  if (!list.length) {
    paymentGates.value = {};
    return;
  }
  const entries = await Promise.all(
    list.map(async (item) => {
      try {
        const check = await checkDocAttachments('PAYMENT', item.id, 'ACTIVATE');
        return [item.id, gateFromCheck(check)] as const;
      } catch (error) {
        console.warn('[payable] 付款回单必传校验查询失败，已降级为“不禁用按钮”', error);
        return [
          item.id,
          degradedGate(error instanceof Error ? error.message : String(error)),
        ] as const;
      }
    }),
  );
  paymentGates.value = Object.fromEntries(entries);
}

// ==================== ATT-09：付款回单上传（挂付款行，不是应付单） ====================

/** 当前在「付款回单」面板里上传的那笔付款（默认选中待执行的那笔） */
const payAttachDocId = ref('');

const payAttachOptions = computed(() =>
  (detail.value?.payments ?? []).map((item) => ({
    value: item.id,
    label: `${item.payment_no} · ${formatMoney(item.amount)} · ${item.approval_status_label}`,
  })),
);

const selectedPaymentLabel = computed(() => {
  const hit = (detail.value?.payments ?? []).find((item) => item.id === payAttachDocId.value);
  return hit ? `${hit.payment_no}（${formatMoney(hit.amount)}，${hit.approval_status_label}）` : '—';
});

/**
 * 详情加载 / 刷新后选一笔付款：优先「待执行」的那笔（出款要的就是它），否则第一笔。
 * 已有有效选择就不动它（用户可能正对着某一笔在传文件）。
 */
function syncPayAttachSelection(): void {
  const list = detail.value?.payments ?? [];
  if (!list.length) {
    payAttachDocId.value = '';
    return;
  }
  if (list.some((item) => item.id === payAttachDocId.value)) {
    return;
  }
  const awaiting = list.find((item) => isAwaitingExecutor(item, instanceStatusOf(item)));
  payAttachDocId.value = awaiting?.id ?? list[0].id;
}


/**
 * 拉「执行付款」判据需要的那一次实例状态（**幂等**，可重复调）。
 *
 * - 后端已下发 `awaiting_executor`（`executorProbeInstanceId` 返回 `null`）→ 什么都不做；
 * - 审批进度块刚拉过同一张实例（缓存命中）→ 复用，不再请求；
 * - 否则 `GET /business/flow/instance/{id}` 一次，失败只记 `UNKNOWN`（按钮不出现，
 *   不能因为审批中心抽风或 403 就显示一个点下去必然 409 的按钮）。
 */
async function loadExecutorGate(row: PayableDetail): Promise<void> {
  const instanceId = executorProbeInstanceId(row.payments);
  if (!instanceId || paymentInstanceStatus.value[instanceId] !== undefined) {
    return;
  }
  try {
    const instance = await getFlowInstance(instanceId);
    cacheInstanceStatus(instanceId, instance?.instance?.status);
  } catch {
    cacheInstanceStatus(instanceId, 'UNKNOWN');
  }
}

/**
 * 点「执行付款」→ 二次确认（钱还没出去，先说清点完会发生什么）。
 *
 * 确认文案里三个事实都不能省：①这笔钱还没付 ②执行后立即应付回写 + 付款凭证
 * ③审批人不能执行自己审批的付款（内控，后端还会再拦一次）。
 */
function openExecutePayment(item: PayablePayment): void {
  if (executingPaymentId.value) {
    return;
  }
  // ATT-09：回单缺失时按钮是灰的（点不到这里）；万一被脚本/键盘触发，先把人送到回单面板
  if (!payGateOf(item).canProceed) {
    payAttachDocId.value = item.id;
    void MessagePlugin.warning(payGateTip(item) || '还缺付款回单，请先在「付款回单」面板上传');
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '执行付款',
    theme: 'warning',
    body: executePaymentConfirmText(item.payment_no, formatMoney(item.amount)),
    confirmBtn: { content: '确认执行', theme: 'primary' },
    cancelBtn: '再想想',
    onConfirm: async () => {
      dialog.hide();
      await doExecutePayment(item);
    },
  });
}

/**
 * 真正执行付款（确认弹窗回调里调）。
 *
 * 成功：提示后端 `data.message`（「付款 PM-xxx 已执行：应付回写 + 付款凭证已生成」）后
 * **从读接口重取**详情与列表 —— 付款记录变「已通过」、未付减少、按钮消失都靠重取，
 * 不信写接口回包（与 `refreshDetail` 同一条口径）。
 *
 * 失败：**原样弹出 `msg`**。409 + `66013`「操作人就是这张付款的审批人」是内控的中文明文，
 * 一个字都不改；409 + `10003`（已执行 / 已终止 / 审批还没通过）说明状态早变了，顺手刷新一次
 * 让按钮跟着真实状态消失。
 */
async function doExecutePayment(item: PayablePayment): Promise<void> {
  if (executingPaymentId.value) {
    return;
  }
  executingPaymentId.value = item.id;
  try {
    const result = await executePayment(item.id);
    MessagePlugin.success(result.message);
    const payableId = detail.value?.id;
    if (payableId) {
      await refreshDetail(payableId);
    }
    void loadPayables();
  } catch (error) {
    MessagePlugin.error(messageOf(error));
    const payableId = detail.value?.id;
    if (payableId) {
      await refreshDetail(payableId);
    }
  } finally {
    executingPaymentId.value = '';
  }
}

// ==================== 付款审批进度（P17） ====================

/** 审批进度（`FlowProgress` 的最小形状，只来自后端节点行）+ 审批人明细（C2 起同传 `tasks`）。 */
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });

/**
 * 进度区标题。
 *
 * P-FLOW-3B 起同一块位置要能显示两种进度：付款审批（P17）与**作废审批**（3B），
 * 标题必须跟着实例类型走 —— 写着「付款审批进度」却在展示作废节点，比不显示更误导。
 */
const approvalTitle = ref('付款审批进度');

/**
 * 拉审批进度。
 *
 * 直接复用审批中心的 `getFlowInstance` + 共享装载器 `shared/approval-progress`
 * （与采购 / 收货 / 凭证 / 调拨同一份代码）：**进度只来自引擎写的节点行**，
 * 前端不自己猜「走到第几步」；`tasks` 是同一接口的 `timeline[]`（谁 / 时间 / 意见）。
 * 拉不到（权限 / 实例已归档）就退化为不显示，不让详情抽屉整块失败。
 */
async function loadApprovalProgressOf(
  instanceId: string | null,
  title = '付款审批进度',
): Promise<void> {
  approvalTitle.value = title;
  // ★ P3-07：这一发顺便把**实例状态**记进缓存 —— 付款「审批通过但还没执行」的判据
  // （开关开着时审批通过后付款仍 PENDING，只有实例落 APPROVED 才说明该有人执行了）。
  // 与 `loadExecutorGate` 共用同一张缓存 → 常态下一次请求都不多。
  approvalProgress.value = await loadApprovalProgress(instanceId, async (id) => {
    const instance = await getFlowInstance(id);
    cacheInstanceStatus(id, instance?.instance?.status);
    return instance;
  });
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

// ==================== 付款 / 作废（P7 → P-FLOW-3B 走审批） ====================

/**
 * 可付款 / 可作废 / 可撤回的判据都在 `payable-logic.ts`（纯函数，单测直接打）。
 *
 * 这里只留模板里的两个别名（`canPay` / `canCancel`），避免改一堆 `v-if`：
 * 作废审批中（`PENDING_CANCEL`）两个都返回 false —— 付款按钮消失、作废按钮换成撤回。
 */

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
    // 写接口回包先落到抽屉（点完立刻有反馈），再**从读接口重取权威快照**：
    // 只信回包里的 detail 会让抽屉停在提交前那一版（P2-2B 手册实测「付款记录 0 笔 / 未付 80.00」），
    // 必须关闭抽屉重开才更新 —— 用户会以为付款没登记上。见 `refreshDetail` 注释。
    detail.value = result.detail;
    await refreshDetail(target.id, result.approval_instance_id);
    void loadPayables();
  } catch (error) {
    payError.value = messageOf(error);
  } finally {
    paySubmitting.value = false;
  }
}

/**
 * 详情抽屉「作废审批中」那句说明。
 *
 * 没撤回权限时把「为什么点不了」讲清楚（V0191 的权限矩阵缺口），而不是只把按钮藏起来；
 * 有权限时说明点完会发生什么（单据回「未付」，欠款不变）。
 */
function pendingCancelDescription(row: PayableRow | PayableDetail | null): string {
  const blocked = withdrawCancelBlockedReason(row, hasWithdrawPermission.value);
  if (blocked) {
    return blocked;
  }
  return '审批通过后单据才变「已作废」并红冲进项凭证；撤回则回到「未付」，可以重新发起作废。';
}

/** 抽屉底部说明要不要显示（能付 / 能作废 / 正在作废审批中都值得看一眼）。 */
const showActionNote = computed(
  () =>
    isPendingCancel(detail.value) ||
    canCancel(detail.value) ||
    canStartPay(payInfo.value?.payable, hasPayPermission.value, payInfo.value?.payable.status ?? detail.value?.status),
);

/**
 * 提交作废（P-FLOW-3B 起**走审批**）。
 *
 * 三种结局，提示必须分开（见 `shared/approval-flow.ts` 的口径表）：
 *
 * - `PENDING_APPROVAL` → **info**「已提交审批，等待财务经理复核」，列表刷新后那行是「作废审批中」；
 * - `CANCELLED` → **success**「已作废：AP-…」（未配流程，降级直做）；
 * - 其它 → 用后端 `message` 兜底。
 *
 * ⚠️ 送审**绝不能**提示「已作废」：欠款一分没动，用户会以为可以重新入库了。
 */
async function submitCancel(): Promise<void> {
  const target = cancelTarget.value;
  if (!target || cancelSubmitting.value) {
    return;
  }
  const reason = cancelForm.reason.trim();
  if (!reason) {
    cancelError.value = '作废原因必填（会计上作废必须留痕，财务经理据此复核）';
    return;
  }
  cancelSubmitting.value = true;
  cancelError.value = '';
  try {
    const result = await submitCancelPayable(target.id, reason);
    cancelVisible.value = false;
    const feedback = submitFeedback('payable-cancel', result, result.payable_no || target.payable_no);
    MessagePlugin[feedback.level](feedback.text);
    // 送审时 detail 是**原单**（状态已变 PENDING_CANCEL），直接作废时是已作废单
    detail.value = result;
    // ★ D5：可付信息重拉（未付没变，但 can_pay / 按钮文案要跟着状态走）
    await loadPayInfo(result.id);
    // 送审的响应是 `PayableDetailRes` 平铺（含 `cancel_instance_id` / `cancel_reason`），
    // 进度 / 原因都从单据字段读，不依赖「送审那一刻」的临时状态
    await refreshApprovalProgress(result);
    void loadPayables();
  } catch (error) {
    cancelError.value = messageOf(error);
  } finally {
    cancelSubmitting.value = false;
  }
}

// ==================== P-FLOW-3B：撤回作废审批 ====================

/** 撤回中（防连点：撤回是不可逆的写操作，双击会拿到 409）。 */
const withdrawingCancel = ref(false);

/**
 * 撤回作废审批：二次确认 → 调接口 → 列表 / 抽屉都回到最新状态。
 *
 * 撤回只清审批单（`PENDING_CANCEL → OPEN`），**账没动**：
 * 所以之后「作废」「付款」按钮都回来了，可以重新提交。
 */
function openWithdrawCancel(row: PayableRow | PayableDetail): void {
  if (!canWithdrawCancel(row, hasWithdrawPermission.value) || withdrawingCancel.value) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '撤回作废审批',
    theme: 'warning',
    body: `${row.payable_no} 的作废审批会被撤回，单据回到「未付」，欠款保持不变。撤回后可以重新发起作废。`,
    confirmBtn: { content: '撤回审批', theme: 'warning' },
    cancelBtn: '再想想',
    onConfirm: async () => {
      dialog.hide();
      await doWithdrawCancel(row);
    },
  });
}

/** 真正执行撤回（确认弹窗的回调里调）。 */
async function doWithdrawCancel(row: PayableRow | PayableDetail): Promise<void> {
  if (withdrawingCancel.value) {
    return;
  }
  withdrawingCancel.value = true;
  try {
    const updated = await withdrawCancelPayable(row.id, '页面撤回');
    const feedback = withdrawFeedback('payable-cancel', updated.payable_no || row.payable_no, payableStatusLabel(updated.status));
    MessagePlugin[feedback.level](feedback.text);
    if (detail.value?.id === updated.id) {
      // 撤回后后端已清掉 `cancel_instance_id` → 进度区跟着回到「付款审批」口径（通常是空）
      detail.value = updated;
      await refreshApprovalProgress(updated);
    }
    await loadPayInfo(updated.id);
    void loadPayables();
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    withdrawingCancel.value = false;
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

/*
 * 区块标题（「付款审批进度」「付款记录」这类）。
 * C2 补：本页原来只有 class 没有样式，标题挤在上一个块的末行 ——
 * 与采购 / 收货 / 凭证 / 调拨的 `section-title` 取同一组值（16px 上边距 + 13px 600）。
 */
.section-title {
  margin: 16px 0 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}
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

/* ★ P-FLOW-3B 补漏：详情抽屉里的「作废原因」（与警告条同色系，不抢主信息） */
.drawer-cancel-reason {
  margin: -4px 0 0;
  font-size: 13px;
  line-height: 22px;
  color: var(--td-text-color-primary);
  word-break: break-all;
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

/*
 * ★ P3-07：付款记录行里的「执行付款」按钮。
 * 该行是 `align-items: baseline` 的 flex 换行行（金额 / 单号 / 状态 / 方式 / 两个链接），
 * 620px 抽屉里按钮基本必然换到第二行 —— 让它在本行垂直居中，别跟 13px 文字挤在基线上。
 */
.payment-row__execute {
  align-self: center;
}

/* ATT-09：disabled 的 t-button 收不到 hover —— tooltip / 红字都必须包在普通元素里 */
.op-wrap {
  display: inline-flex;
  align-self: center;
}

/* ATT-09：付款回单面板（多笔付款时选哪一笔） */
.pay-attach-select {
  max-width: 420px;
  margin-bottom: 6px;
}

.pay-attach-tip {
  margin: 0 0 8px;
  font-size: 12px;
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
