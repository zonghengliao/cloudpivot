<template>
  <div class="inv-page">
    <!-- ==================== ① 筛选条 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-inv-refresh" @click="refresh">
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
          data-testid="inv-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.invoiceType"
          class="tool-row__type"
          :options="typeOptions"
          placeholder="全部票种"
          clearable
          data-testid="inv-filter-type"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.invoiceNo"
          class="tool-row__no"
          placeholder="发票号 / 客户名（模糊）"
          clearable
          data-testid="inv-filter-no"
          @enter="handleSearch"
        />
      </div>
      <div class="tool-row tool-row--second">
        <span class="tool-row__label">开票日期</span>
        <t-date-picker
          v-model="filter.dateFrom"
          class="tool-row__date"
          value-type="YYYY-MM-DD"
          allow-input
          clearable
          placeholder="起（含）"
          data-testid="inv-filter-date-from"
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
          data-testid="inv-filter-date-to"
          @change="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-inv-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-inv-reset" @click="handleReset">重置</t-button>
        <t-tag v-if="isInvoiceFiltered(filter)" size="small" variant="light" theme="primary">
          已筛选
        </t-tag>

        <span class="tool-row__spacer" />

        <t-tooltip :content="createTip" :disabled="!createTip" placement="top-right">
          <span class="op-wrap">
            <t-button
              theme="primary"
              :disabled="!hasCreatePermission"
              data-testid="btn-inv-create"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              开票
            </t-button>
          </span>
        </t-tooltip>
      </div>
    </t-card>

    <!-- ==================== ② 发票列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">销售发票</span>
          <span class="panel-head__meta" data-testid="inv-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">
          草稿 → 开票（生成收入凭证 借 1122 / 贷 5001 + 2221.02，并回写应收发票号）→ 归档；
          DRAFT / ISSUED 可作废（ISSUED 作废生成红字凭证）
        </span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="inv-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadInvoices()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="INVOICE_COLUMNS"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="inv-table"
        @page-change="handlePageChange"
      >
        <template #invoice_no="{ row }">
          <div class="no-cell">
            <t-link
              theme="primary"
              hover="color"
              :data-testid="`inv-no-${row.invoice_no}`"
              @click="openDetail(row)"
            >
              <span class="mono">{{ row.invoice_no }}</span>
            </t-link>
            <t-tag
              v-if="row.invoice_type !== 'OUT'"
              size="small"
              variant="light"
              theme="warning"
              :data-testid="`inv-type-${row.invoice_no}`"
            >
              {{ invoiceTypeLabel(row) }}
            </t-tag>
          </div>
        </template>

        <template #receivable_no="{ row }">
          <t-link
            v-if="arNoOf(row)"
            theme="primary"
            hover="color"
            :data-testid="`inv-ar-link-${row.invoice_no}`"
            :title="`跳到应收台账并筛出 ${arNoOf(row)}`"
            @click="jumpToReceivable(row)"
          >
            <span class="mono">{{ arNoOf(row) }}</span>
          </t-link>
          <span
            v-else-if="row.receivable_id && arNoOf(row) === undefined"
            class="muted"
            :data-testid="`inv-ar-loading-${row.invoice_no}`"
            title="应收单号按需解析中（列表接口只回 receivable_id）"
          >
            …
          </span>
          <span
            v-else-if="row.receivable_id"
            class="muted"
            :data-testid="`inv-ar-missing-${row.invoice_no}`"
          >
            应收已删除
          </span>
          <span v-else class="muted" data-testid="inv-ar-none">手工开票</span>
        </template>

        <template #customer_name="{ row }">
          <span :class="{ muted: !row.customer_name }" :title="row.customer_name || row.customer_id">
            {{ row.customer_name || `（客户已删除 ${shortId(row.customer_id)}）` }}
          </span>
        </template>

        <template #invoice_date="{ row }">
          <span :data-testid="`inv-date-${row.invoice_no}`">{{ dash(row.invoice_date) }}</span>
        </template>

        <template #amount="{ row }">
          <span class="money" :data-testid="`inv-amount-${row.invoice_no}`">
            {{ formatMoney(row.amount) }}
          </span>
        </template>

        <template #tax_amount="{ row }">
          <span class="money money--tax" :data-testid="`inv-tax-${row.invoice_no}`">
            {{ formatMoney(row.tax_amount) }}
          </span>
        </template>

        <template #total_with_tax="{ row }">
          <span class="money money--strong" :data-testid="`inv-total-${row.invoice_no}`">
            {{ formatMoney(row.total_with_tax) }}
          </span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="invoiceStatusTheme(row.status)"
            :title="`状态编码：${row.status}`"
            :data-testid="`inv-status-${row.invoice_no}`"
          >
            {{ invoiceStatusLabel(row) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <t-button
            size="small"
            variant="text"
            :data-testid="`inv-op-detail-${row.invoice_no}`"
            @click="openDetail(row)"
          >
            详情
          </t-button>

          <t-tooltip
            v-if="row.status === 'DRAFT'"
            :content="statusTip"
            :disabled="!statusTip"
            placement="top-left"
          >
            <span class="op-wrap">
              <t-button
                size="small"
                variant="text"
                theme="primary"
                :disabled="!hasStatusPermission"
                :data-testid="`inv-op-issue-${row.invoice_no}`"
                @click="issueInvoice(row)"
              >
                开票
              </t-button>
            </span>
          </t-tooltip>

          <t-tooltip
            v-if="row.status === 'ISSUED'"
            :content="statusTip"
            :disabled="!statusTip"
            placement="top-left"
          >
            <span class="op-wrap">
              <t-button
                size="small"
                variant="text"
                :disabled="!hasStatusPermission"
                :data-testid="`inv-op-archive-${row.invoice_no}`"
                @click="archiveInvoice(row)"
              >
                归档
              </t-button>
            </span>
          </t-tooltip>

          <t-tooltip
            v-if="row.status === 'DRAFT' || row.status === 'ISSUED'"
            :content="voidTip(row) || statusTip"
            :disabled="!voidTip(row) && !statusTip"
            placement="top-left"
          >
            <span class="op-wrap">
              <t-button
                size="small"
                variant="text"
                theme="danger"
                :disabled="!hasStatusPermission"
                :data-testid="`inv-op-void-${row.invoice_no}`"
                @click="openVoid(row)"
              >
                作废
              </t-button>
            </span>
          </t-tooltip>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 开票弹窗 ==================== -->
    <t-dialog
      v-model:visible="createVisible"
      header="开票（从应收）"
      width="640px"
      :confirm-btn="{ content: '确认开票', loading: createSubmitting, theme: 'primary' }"
      :close-on-overlay-click="!createSubmitting"
      data-testid="inv-create-dialog"
      @confirm="submitCreate"
    >
      <div class="dialog-body">
        <t-alert
          theme="info"
          message="选一张应收 → 客户 / 来源 / 金额自动带出；价税合计缺省 = 该应收的剩余可开票额度，可改小做分次开票。"
        />

        <t-form label-align="top" class="form">
          <t-form-item label="应收单" required-mark>
            <t-select
              v-model="createForm.receivableId"
              :options="receivableOptions"
              :loading="receivableLoading"
              placeholder="选择应收单（只列未收清的 OPEN / PARTIAL）"
              filterable
              clearable
              data-testid="inv-create-receivable"
              @change="handleReceivableChange"
            />
            <p v-if="receivableError" class="form-tip form-tip--error" data-testid="inv-create-ar-error">
              {{ receivableError }}
            </p>
          </t-form-item>

          <!-- 选中应收后的实时额度面板：金额 / 已开票 / 可开票 -->
          <div v-if="selectedReceivable" class="ar-panel" data-testid="inv-create-ar-panel">
            <div class="ar-panel__row">
              <span class="ar-panel__label">客户</span>
              <span class="ar-panel__value">{{ selectedReceivable.customer_name || '（客户已删除）' }}</span>
            </div>
            <div class="ar-panel__row">
              <span class="ar-panel__label">应收金额</span>
              <span class="ar-panel__value money">{{ formatMoney(selectedReceivable.amount) }}</span>
            </div>
            <div class="ar-panel__row">
              <span class="ar-panel__label">已开票价税合计</span>
              <span class="ar-panel__value money" data-testid="inv-create-invoiced">
                {{ formatCents(invoicedCents) }}
              </span>
            </div>
            <div class="ar-panel__row">
              <span class="ar-panel__label">剩余可开票额度</span>
              <span
                class="ar-panel__value money money--strong"
                :class="{ 'text-danger': headroomCents <= 0 }"
                data-testid="inv-create-headroom"
              >
                {{ formatCents(headroomCents) }}
              </span>
            </div>
            <p v-if="headroomLoading" class="form-tip">正在核对该应收的已开票金额…</p>
          </div>

          <!-- 「创建后立即开票」紧贴应收面板：这是本弹窗**最主要的开关**，
               放到表单末尾会被 64vh 的滚动区藏起来（实测 1000px 视口看不到） -->
          <t-checkbox
            v-model="createForm.issueNow"
            class="issue-now"
            data-testid="inv-create-issue-now"
          >
            创建后立即开票（生成收入凭证并回写应收发票号，跳过草稿）
          </t-checkbox>

          <div class="form-grid">
            <t-form-item label="开票日期">
              <t-date-picker
                v-model="createForm.invoiceDate"
                value-type="YYYY-MM-DD"
                allow-input
                clearable
                placeholder="缺省今天"
                data-testid="inv-create-date"
              />
            </t-form-item>
            <t-form-item label="税率">
              <t-input-number
                v-model="createForm.taxRate"
                :min="0"
                :max="100"
                :step="1"
                :decimal-places="2"
                theme="normal"
                suffix="%"
                data-testid="inv-create-rate"
              />
            </t-form-item>
          </div>
          <p class="form-tip">税率按<strong>百分数</strong>填（<code>13</code> = 13%，与订单 / 收货同口径）；当前 {{ taxRateText(createForm.taxRate) }}</p>

          <t-form-item label="价税合计" required-mark>
            <t-input-number
              v-model="createForm.total"
              :min="0"
              :step="100"
              theme="normal"
              suffix="元"
              data-testid="inv-create-total"
            />
            <p class="form-tip" data-testid="inv-create-split">
              不含税 <span class="money">{{ formatCents(previewSplit.amountCents) }}</span>
              ＋ 税额 <span class="money">{{ formatCents(previewSplit.taxCents) }}</span>
              = 合计 <span class="money">{{ formatCents(toCents(createForm.total)) }}</span>
              （预览；权威金额以后端 4 位定标结果为准）
            </p>
          </t-form-item>

          <div class="form-grid">
            <t-form-item label="发票号">
              <t-input
                v-model="createForm.invoiceNo"
                placeholder="留空自动取 IV-yyyyMMdd-000001"
                clearable
                data-testid="inv-create-no"
              />
            </t-form-item>
            <t-form-item label="发票代码">
              <t-input
                v-model="createForm.invoiceCode"
                placeholder="可空（税控发票代码）"
                clearable
                data-testid="inv-create-code"
              />
            </t-form-item>
          </div>

          <!-- 票面形式：决定「开票」时发票影像的格式白名单（电子票 pdf/ofd/xml、纸质票 jpg/png/pdf），
               后端 biz_invoice.is_electronic 落库，附件必传规则按它分支（ATT-07 / V0348） -->
          <t-form-item label="票面形式" required-mark>
            <t-radio-group
              v-model="createForm.isElectronic"
              :options="faceOptions"
              data-testid="inv-create-face"
            />
            <p class="form-tip" data-testid="inv-create-face-hint">
              {{ invoiceAttachmentHint(createForm.isElectronic) }}
            </p>
          </t-form-item>

          <t-form-item label="备注">
            <t-input
              v-model="createForm.remark"
              placeholder="可空"
              clearable
              data-testid="inv-create-remark"
            />
          </t-form-item>
        </t-form>

        <t-alert
          v-if="createError"
          theme="error"
          :message="createError"
          class="dialog-alert"
          data-testid="inv-create-error"
        />
      </div>
    </t-dialog>

    <!-- ==================== ④ 作废弹窗 ==================== -->
    <t-dialog
      v-model:visible="voidVisible"
      header="作废发票"
      width="560px"
      :confirm-btn="{ content: '确认作废', loading: voidSubmitting, theme: 'danger' }"
      :close-on-overlay-click="!voidSubmitting"
      data-testid="inv-void-dialog"
      @confirm="submitVoid"
    >
      <div class="dialog-body">
        <div v-if="voidTarget" class="void-target" data-testid="inv-void-target">
          <span class="mono">{{ voidTarget.invoice_no }}</span>
          <span class="muted">·</span>
          <span>{{ voidTarget.customer_name || '（客户已删除）' }}</span>
          <span class="muted">·</span>
          <span class="money money--strong">{{ formatMoney(voidTarget.total_with_tax) }}</span>
          <t-tag size="small" variant="light" :theme="invoiceStatusTheme(voidTarget.status)">
            {{ invoiceStatusLabel(voidTarget) }}
          </t-tag>
        </div>

        <!-- 影响摘要单独一块：`t-alert` 同时传 message 时 `#description` 插槽不渲染（SO-15 踩过） -->
        <div v-if="voidTarget" class="void-impact" data-testid="inv-void-impact">
          {{ voidImpactText(voidTarget) }}
        </div>

        <t-form label-align="top" class="form">
          <t-form-item label="作废原因" required-mark>
            <t-textarea
              v-model="voidReason"
              :maxlength="INVOICE_REASON_MAX"
              :autosize="{ minRows: 3, maxRows: 5 }"
              placeholder="至少 4 个字：为什么作废这张发票（审计要能回答）"
              data-testid="inv-void-reason"
            />
            <p
              v-if="voidReasonErrorText"
              class="form-tip form-tip--error"
              data-testid="inv-void-reason-error"
            >
              {{ voidReasonErrorText }}
            </p>
          </t-form-item>
        </t-form>

        <t-alert
          v-if="voidError"
          theme="error"
          :message="voidError"
          class="dialog-alert"
          data-testid="inv-void-error"
        />
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="520px"
      :header="detail ? `发票 ${detail.invoice_no}` : '发票详情'"
      :footer="false"
      data-testid="inv-detail"
    >
      <div v-if="detailLoading" class="detail-loading">加载中…</div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" data-testid="inv-detail-error" />

      <div v-else-if="detail" class="detail" data-testid="inv-detail-body">
        <div class="detail-head">
          <span class="detail-head__no mono" data-testid="inv-detail-no">{{ detail.invoice_no }}</span>
          <t-tag
            size="small"
            variant="light"
            :theme="invoiceStatusTheme(detail.status)"
            data-testid="inv-detail-status"
          >
            {{ invoiceStatusLabel(detail) }}
          </t-tag>
          <t-tag size="small" variant="outline">{{ invoiceTypeLabel(detail) }}</t-tag>
        </div>

        <p class="detail-hint" data-testid="inv-detail-hint">{{ nextActionHint(detail) }}</p>

        <div class="detail-grid">
          <div class="detail-item">
            <span class="detail-item__label">客户</span>
            <span class="detail-item__value">{{ detail.customer_name || '（客户已删除）' }}</span>
          </div>
          <div class="detail-item">
            <span class="detail-item__label">开票日期</span>
            <span class="detail-item__value">{{ dash(detail.invoice_date) }}</span>
          </div>
          <div class="detail-item">
            <span class="detail-item__label">发票代码</span>
            <span class="detail-item__value mono">{{ dash(detail.invoice_code) }}</span>
          </div>
          <div class="detail-item">
            <span class="detail-item__label">票面形式</span>
            <span class="detail-item__value" data-testid="inv-detail-face">
              {{ detail.is_electronic ? '电子票（数电票）' : '纸质票（扫描件）' }}
            </span>
          </div>
          <div class="detail-item">
            <span class="detail-item__label">税率</span>
            <span class="detail-item__value">{{ taxRateText(detail.tax_rate) }}</span>
          </div>
          <div class="detail-item detail-item--money">
            <span class="detail-item__label">不含税金额</span>
            <span class="detail-item__value money">{{ formatMoney(detail.amount) }}</span>
          </div>
          <div class="detail-item detail-item--money">
            <span class="detail-item__label">税额</span>
            <span class="detail-item__value money">{{ formatMoney(detail.tax_amount) }}</span>
          </div>
          <div class="detail-item detail-item--money">
            <span class="detail-item__label">价税合计</span>
            <span class="detail-item__value money money--strong">
              {{ formatMoney(detail.total_with_tax) }}
            </span>
          </div>
          <div class="detail-item">
            <span class="detail-item__label">来源</span>
            <span class="detail-item__value" data-testid="inv-detail-source">
              {{ invoiceSourceText(detail) }}
            </span>
          </div>
        </div>

        <div class="detail-section">
          <div class="detail-section__title">关联单据</div>
          <div class="detail-item">
            <span class="detail-item__label">应收单</span>
            <span class="detail-item__value">
              <t-link
                v-if="detail.receivable_no"
                theme="primary"
                hover="color"
                data-testid="inv-detail-ar-link"
                @click="jumpToReceivable(detail)"
              >
                <span class="mono">{{ detail.receivable_no }}</span>
              </t-link>
              <span v-else class="muted">手工开票（无关联应收）</span>
            </span>
          </div>
          <div v-if="detail.receivable_no" class="detail-item">
            <span class="detail-item__label">应收金额</span>
            <span class="detail-item__value money">{{ formatMoney(detail.receivable_amount) }}</span>
          </div>
          <div v-if="detail.receivable_no" class="detail-item">
            <span class="detail-item__label">应收状态</span>
            <span class="detail-item__value">
              {{ detail.receivable_status_label || dash(detail.receivable_status) }}
            </span>
          </div>
          <div v-if="detail.receivable_no" class="detail-item">
            <span class="detail-item__label">剩余可开票额度</span>
            <span class="detail-item__value money" data-testid="inv-detail-headroom">
              {{ formatMoney(detail.receivable_headroom) }}
            </span>
          </div>
          <div class="detail-item">
            <span class="detail-item__label">收入凭证</span>
            <span class="detail-item__value mono" data-testid="inv-detail-voucher">
              {{ dash(detail.voucher_no) }}
            </span>
          </div>
          <div class="detail-item">
            <span class="detail-item__label">备注</span>
            <span class="detail-item__value">{{ dash(detail.remark) }}</span>
          </div>
          <div class="detail-item">
            <span class="detail-item__label">创建 / 更新</span>
            <span class="detail-item__value muted">
              {{ dateTimeText(detail.created_at) }} / {{ dateTimeText(detail.updated_at) }}
            </span>
          </div>
        </div>

        <div class="detail-actions" data-testid="inv-detail-actions">
          <t-button
            v-if="detail.status === 'DRAFT'"
            theme="primary"
            :disabled="!hasStatusPermission"
            data-testid="inv-detail-issue"
            @click="issueInvoice(detail)"
          >
            开票（生成收入凭证）
          </t-button>
          <t-button
            v-if="detail.status === 'ISSUED'"
            :disabled="!hasStatusPermission"
            data-testid="inv-detail-archive"
            @click="archiveInvoice(detail)"
          >
            归档
          </t-button>
          <t-button
            v-if="detail.status === 'DRAFT' || detail.status === 'ISSUED'"
            theme="danger"
            variant="outline"
            :disabled="!hasStatusPermission"
            data-testid="inv-detail-void"
            @click="openVoid(detail)"
          >
            作废
          </t-button>
          <t-button data-testid="btn-inv-drawer-close" @click="detailVisible = false">关闭</t-button>
        </div>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 销售发票页（SO-21）—— 后端 SO-20（`backend/src/business/erp/invoice.rs`，4 个接口）。
 *
 * # 口径来源（逐字抄后端，不猜）
 *
 * | 语义 | 出处 |
 * |------|------|
 * | 列表 / 详情行 | `invoice::InvoiceRes` / `InvoiceDetailRes`（`@/api/business/erp` 的 `Invoice` / `InvoiceDetail`） |
 * | 分页入参 | `invoice::InvoicePageQuery`（`invoice_no` 是**模糊**匹配单号+客户名；空串一律不发） |
 * | 状态域 | `invoice::STATUSES` = DRAFT / ISSUED / POSTED / VOID（**不是**收款的 CANCELLED） |
 * | 状态机 | `state_machine::INVOICE_EDGES`（4 条：没有 POSTED→VOID、没有回 DRAFT） |
 * | 开票 | `PATCH /invoice/{id}/status` → ISSUED：生成收入凭证（借 1122 / 贷 5001 + 2221.02）+ 回写应收 |
 * | 作废 | → VOID，原因 ≥4 字；ISSUED 来的必红冲（`reverse_posted` 红字凭证）+ 撤回应收发票号 |
 * | 额度 | `SUM(total_with_tax) WHERE receivable_id=? AND status<>'VOID'` ≤ 应收金额（409 点名三数） |
 *
 * # 「自动带出金额」怎么落地
 *
 * 选完应收后**只发一次** `GET /invoice/page?receivable_id=…`，用真实发票算出
 * 「已开票价税合计」，剩余额度 = 应收金额 − 已开票（排除 VOID，与后端 `invoiced_total`
 * 同口径）。价税合计缺省填剩余额度，用户可改小做分次开票。**不在前端硬编
 * 「= 应收金额」**：同一张应收常常要开两张票（分次开、部分红冲），
 * 直接填全额会让第二次开票必然 409。
 *
 * # 金额为什么走整数分
 *
 * 后端 `Decimal` 出 JSON 是字符串（`"1130.0000"`），前端 `+` 会变成字符串拼接。
 * 额度、预览税额全部经 `views/business/erp/shared/money.ts` 的 `toCents`，
 * 只在显示前换回元。纯逻辑与单测在 `./invoice-logic.ts`。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';

import {
  changeInvoiceStatus,
  createInvoice,
  fetchInvoiceDetail,
  fetchInvoicePage,
  fetchReceivableDetail,
  fetchReceivablePage,
  type Invoice,
  type InvoiceDetail,
  type Receivable,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';

import { dash, formatCents, formatMoney, shortId, toCents, todayString } from '../shared/money';
import {
  INVOICE_COLUMNS,
  INVOICE_DEFAULT_TAX_RATE,
  INVOICE_FACE_OPTIONS,
  INVOICE_PAGE_SIZE,
  INVOICE_PERMS,
  INVOICE_REASON_MAX,
  INVOICE_STATUS_OPTIONS,
  INVOICE_TYPE_OPTIONS,
  RECEIVABLE_FETCH_LIMIT,
  buildInvoiceQuery,
  emptyInvoiceFilter,
  invoiceAttachmentHint,
  invoiceFormError,
  invoiceReasonError,
  invoiceSourceText,
  invoiceStatusLabel,
  invoiceStatusTheme,
  invoiceTypeLabel,
  invoicedTotalCents,
  isInvoiceFiltered,
  nextActionHint,
  receivableOptionLabel,
  splitTaxCents,
  taxRateText,
  voidBlockedReason,
  voidImpactText,
  type InvoiceFilterState,
  type InvoiceFormState,
} from './invoice-logic';

const router = useRouter();
const userStore = useUserStore();

const statusOptions = INVOICE_STATUS_OPTIONS.map((item) => ({ ...item }));
const typeOptions = INVOICE_TYPE_OPTIONS.map((item) => ({ ...item }));
/** 票面形式单选（电子票 / 纸质票）—— 逐字来自 `invoice-logic.ts`，与后端 is_electronic 同序 */
const faceOptions = INVOICE_FACE_OPTIONS.map((item) => ({ ...item }));

const hasCreatePermission = computed(() => userStore.hasPermission(INVOICE_PERMS.create));
const hasStatusPermission = computed(() => userStore.hasPermission(INVOICE_PERMS.status));

const createTip = computed(() => (hasCreatePermission.value ? '' : `缺少权限 ${INVOICE_PERMS.create}`));
const statusTip = computed(() => (hasStatusPermission.value ? '' : `缺少权限 ${INVOICE_PERMS.status}`));

// ==================== 列表 ====================

const filter = reactive<InvoiceFilterState>(emptyInvoiceFilter());
const rows = ref<Invoice[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = INVOICE_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

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

async function loadInvoices(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchInvoicePage(buildInvoiceQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    const rawPage = data.page ?? page;
    const maxPage = Math.max(1, Math.ceil(total.value / pageSize));
    current.value = Math.min(Math.max(1, rawPage), maxPage);
    // 关联应收单号按需补（列表接口只回 receivable_id，见 resolveReceivableNos）
    void resolveReceivableNos(rows.value);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = messageOf(error);
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  void loadInvoices(1);
}

function handleReset(): void {
  Object.assign(filter, emptyInvoiceFilter());
  void loadInvoices(1);
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  void loadInvoices(pageInfo.current);
}

function refresh(): void {
  void loadInvoices(current.value);
}

/** 作废按钮的置灰原因（权限优先，其次状态机） */
function voidTip(row: Pick<Invoice, 'status'>): string {
  if (!hasStatusPermission.value) return `缺少权限 ${INVOICE_PERMS.status}`;
  return voidBlockedReason(row);
}

/**
 * 列表里的「关联应收」单号为什么要前端解析
 *
 * `invoice::InvoiceRes`（列表行）只下发 `receivable_id`，**不带** `receivable_no`
 * —— 单号是详情（`InvoiceDetailRes`）才有的。SO-21 的卡面要求列表能点着单号跳转，
 * 又不能在发票卡里改后端契约（SO-20 的 4 接口已 curl 验证并提交），
 * 所以在当前页**按 distinct 的 `receivable_id` 调 `GET /receivable/{id}`** 补单号：
 * 一页最多 20 个 id、结果进 `arNoById` 缓存跨页复用，刷新不再重复请求。
 *
 * 三态要分开：`undefined` = 还在解析（列表里显示 `…`）、`string` = 单号、
 * `null` = 应收已删（显示「应收已删除」而不是误导成「手工开票」）。
 */
const arNoById = ref<Record<string, string | null>>({});

function arNoOf(row: Pick<Invoice, 'receivable_id'>): string | null | undefined {
  if (!row.receivable_id) return null;
  return arNoById.value[row.receivable_id];
}

async function resolveReceivableNos(list: Invoice[]): Promise<void> {
  const pending = [
    ...new Set(
      list
        .map((item) => item.receivable_id)
        .filter((id): id is string => Boolean(id) && arNoById.value[id as string] === undefined),
    ),
  ].slice(0, 40);
  if (!pending.length) return;
  const resolved = await Promise.all(
    pending.map(async (id) => {
      try {
        const detail = await fetchReceivableDetail(id);
        return [id, detail.ar_no] as const;
      } catch {
        // 应收被删 / 无权看：标记成「已删除」并缓存，避免每次翻页重试
        return [id, null] as const;
      }
    }),
  );
  arNoById.value = { ...arNoById.value, ...Object.fromEntries(resolved) };
}

/** 跳到应收台账并筛出这张应收（应收页读 `?ar_no=`，见 SO-15 页的 deep-link） */
function jumpToReceivable(row: { receivable_id?: string | null; receivable_no?: string | null }): void {
  const arNo = (row.receivable_no ?? (row.receivable_id ? arNoById.value[row.receivable_id] : null) ?? '').trim();
  if (!arNo) return;
  void router.push({ path: '/business/erp/receivable', query: { ar_no: arNo } });
}

// ==================== 开票弹窗 ====================

const createVisible = ref(false);
const createSubmitting = ref(false);
const createError = ref('');
const receivableLoading = ref(false);
const receivableError = ref('');
const headroomLoading = ref(false);

/** 可开票的应收（未收清的 OPEN / PARTIAL 两档，按到期日升序） */
const receivables = ref<Receivable[]>([]);
const receivableOptions = computed(() =>
  receivables.value.map((item) => ({ label: receivableOptionLabel(item), value: item.id })),
);

const createForm = reactive<InvoiceFormState & { issueNow: boolean }>({
  receivableId: '',
  invoiceDate: todayString(),
  total: 0,
  taxRate: INVOICE_DEFAULT_TAX_RATE,
  invoiceNo: '',
  invoiceCode: '',
  isElectronic: true,
  remark: '',
  issueNow: true,
});

/** 当前选中应收的已开票价税合计（整数分；选完应收后由 `GET /invoice/page?receivable_id=` 算出） */
const invoicedCents = ref(0);

const selectedReceivable = computed(
  () => receivables.value.find((item) => item.id === createForm.receivableId) ?? null,
);

const headroomCents = computed(() =>
  selectedReceivable.value
    ? toCents(selectedReceivable.value.amount) - invoicedCents.value
    : 0,
);

const previewSplit = computed(() => splitTaxCents(toCents(createForm.total), Number(createForm.taxRate)));

/**
 * 拉一次可开票应收：`status` 只收单值，而「未收清」在后端是 OPEN 与 PARTIAL 两种
 * （`receivable::has_balance`），所以打两次再按 id 去重、按到期日升序。
 */
async function loadReceivables(): Promise<void> {
  receivableLoading.value = true;
  receivableError.value = '';
  try {
    const [openPage, partialPage] = await Promise.all([
      fetchReceivablePage({ page: 1, page_size: RECEIVABLE_FETCH_LIMIT, status: 'OPEN' }),
      fetchReceivablePage({ page: 1, page_size: RECEIVABLE_FETCH_LIMIT, status: 'PARTIAL' }),
    ]);
    const merged = new Map<string, Receivable>();
    for (const item of [...(openPage.list ?? []), ...(partialPage.list ?? [])]) {
      merged.set(item.id, item);
    }
    receivables.value = [...merged.values()].sort((a, b) => {
      if (a.due_date === b.due_date) return a.ar_no.localeCompare(b.ar_no);
      return a.due_date < b.due_date ? -1 : 1;
    });
  } catch (error) {
    receivables.value = [];
    receivableError.value = `应收单列表加载失败：${messageOf(error)}`;
  } finally {
    receivableLoading.value = false;
  }
}

/** 选中应收 → 拉它的发票算已开票额度 → 自动带出价税合计 */
async function handleReceivableChange(value: unknown): Promise<void> {
  const receivableId = typeof value === 'string' ? value : '';
  invoicedCents.value = 0;
  createError.value = '';
  const ar = receivables.value.find((item) => item.id === receivableId);
  if (!ar) {
    createForm.total = 0;
    return;
  }
  headroomLoading.value = true;
  try {
    // 真实接口算「已开票」：与后端 `invoiced_total` 同口径（排除 VOID）
    const data = await fetchInvoicePage({
      page: 1,
      page_size: 200,
      receivable_id: ar.id,
    });
    invoicedCents.value = invoicedTotalCents(data.list ?? []);
  } catch (error) {
    // 算不出来也别挡住开票（后端建单时会用同一把锁再判一次额度）
    invoicedCents.value = 0;
    createError.value = `已开票金额查询失败（不影响继续开票，后端仍会校验额度）：${messageOf(error)}`;
  } finally {
    headroomLoading.value = false;
  }
  const headroom = toCents(ar.amount) - invoicedCents.value;
  createForm.total = Math.max(0, headroom) / 100;
}

function openCreate(): void {
  if (!hasCreatePermission.value) {
    MessagePlugin.warning(createTip.value);
    return;
  }
  Object.assign(createForm, {
    receivableId: '',
    invoiceDate: todayString(),
    total: 0,
    taxRate: INVOICE_DEFAULT_TAX_RATE,
    invoiceNo: '',
    invoiceCode: '',
    isElectronic: true,
    remark: '',
    issueNow: true,
  });
  invoicedCents.value = 0;
  createError.value = '';
  createVisible.value = true;
  void loadReceivables();
}

async function submitCreate(): Promise<void> {
  const localError = invoiceFormError(createForm, {
    receivableAmount: selectedReceivable.value?.amount ?? 0,
    invoicedCents: invoicedCents.value,
    hasReceivable: Boolean(selectedReceivable.value),
  });
  if (localError) {
    createError.value = localError;
    MessagePlugin.warning(localError);
    return;
  }
  createSubmitting.value = true;
  createError.value = '';
  try {
    const created = await createInvoice({
      receivable_id: createForm.receivableId,
      invoice_date: createForm.invoiceDate || null,
      // 后端 Decimal 收字符串：定点 4 位，避免 0.1 的二进制尾巴
      total_with_tax: (toCents(createForm.total) / 100).toFixed(4),
      tax_rate: Number(createForm.taxRate),
      invoice_no: createForm.invoiceNo.trim() || null,
      invoice_code: createForm.invoiceCode.trim() || null,
      // 票面形式决定开票时发票影像的格式白名单（电子票 pdf/ofd/xml / 纸质票 jpg/png/pdf）
      is_electronic: createForm.isElectronic,
      remark: createForm.remark.trim() || null,
    });
    createVisible.value = false;

    if (!createForm.issueNow) {
      MessagePlugin.success(`${created.invoice_no} 已建草稿（DRAFT），核对后再点「开票」`);
      await loadInvoices(1);
      return;
    }

    // 「创建后立即开票」：草稿 + ISSUED 两步都成功才算开票完成；
    // 第二步失败时**草稿已经落库**，必须如实告诉用户去哪重试（别报「失败」让他重开一张）。
    try {
      const issued = await changeInvoiceStatus(created.id, 'ISSUED');
      MessagePlugin.success(
        `${issued.invoice_no} 已开票：收入凭证 ${issued.voucher_no ?? '已生成'}，应收 ${issued.receivable_no ?? ''} 已回写发票号`,
      );
      await loadInvoices(1);
      await openDetail(issued);
    } catch (issueError) {
      MessagePlugin.warning(
        `${created.invoice_no} 已建草稿，但开票没成功：${messageOf(issueError)}（可在列表里对该草稿重试「开票」）`,
      );
      await loadInvoices(1);
    }
  } catch (error) {
    createError.value = messageOf(error);
    MessagePlugin.error(createError.value);
  } finally {
    createSubmitting.value = false;
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<InvoiceDetail | null>(null);

function dateTimeText(value: string | null | undefined): string {
  if (!value) return '—';
  // 后端 `created_at` 是 `DateTime<Utc>`（`…Z`），直接切字符串会显示 UTC（比北京时间早 8 小时）
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return value.replace('T', ' ').slice(0, 19);
  const cst = new Date(parsed.getTime() + 8 * 3600_000);
  return cst.toISOString().replace('T', ' ').slice(0, 19);
}

async function openDetail(row: Pick<Invoice, 'id'>): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  detailError.value = '';
  detail.value = null;
  try {
    detail.value = await fetchInvoiceDetail(row.id);
  } catch (error) {
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

// ==================== 状态动作（开票 / 归档） ====================

const statusBusy = ref('');

/**
 * 跑一条状态边。
 *
 * 成功后：刷新列表 + 详情（`detailVisible` 时），并把 `actionError` 清空 ——
 * 上一次失败的提示不能挂在成功之后（SO-15 踩过：红条与「已提交」同时可见）。
 */
async function runStatus(row: Pick<Invoice, 'id'>, target: 'ISSUED' | 'POSTED'): Promise<void> {
  // 防连点：同一次交互里再点一次会拿到 409「已经是…」，看着像功能坏了
  if (statusBusy.value) return;
  statusBusy.value = `${row.id}:${target}`;
  try {
    const updated = await changeInvoiceStatus(row.id, target);
    MessagePlugin.success(
      target === 'ISSUED'
        ? `${updated.invoice_no} 已开票：凭证 ${updated.voucher_no ?? '已生成'}，应收发票号已回写`
        : `${updated.invoice_no} 已归档（终态）`,
    );
    await loadInvoices(current.value);
    if (detailVisible.value && detail.value?.id === updated.id) {
      detail.value = updated;
    }
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  } finally {
    statusBusy.value = '';
  }
}

function issueInvoice(row: Pick<Invoice, 'id' | 'invoice_no'>): void {
  if (!hasStatusPermission.value) {
    MessagePlugin.warning(statusTip.value);
    return;
  }
  void runStatus(row, 'ISSUED');
}

function archiveInvoice(row: Pick<Invoice, 'id' | 'invoice_no'>): void {
  if (!hasStatusPermission.value) {
    MessagePlugin.warning(statusTip.value);
    return;
  }
  void runStatus(row, 'POSTED');
}

// ==================== 作废 ====================

const voidVisible = ref(false);
const voidSubmitting = ref(false);
const voidTarget = ref<Invoice | InvoiceDetail | null>(null);
const voidReason = ref('');
const voidError = ref('');

const voidReasonErrorText = computed(() => voidReason.value ? invoiceReasonError(voidReason.value) : '');

function openVoid(row: Invoice | InvoiceDetail): void {
  if (!hasStatusPermission.value) {
    MessagePlugin.warning(statusTip.value);
    return;
  }
  const blocked = voidBlockedReason(row);
  if (blocked) {
    MessagePlugin.warning(blocked);
    return;
  }
  voidTarget.value = row;
  voidReason.value = '';
  voidError.value = '';
  voidVisible.value = true;
}

async function submitVoid(): Promise<void> {
  const target = voidTarget.value;
  if (!target) return;
  const reasonError = invoiceReasonError(voidReason.value);
  if (reasonError) {
    voidError.value = reasonError;
    MessagePlugin.warning(reasonError);
    return;
  }
  voidSubmitting.value = true;
  voidError.value = '';
  try {
    const updated = await changeInvoiceStatus(target.id, 'VOID', voidReason.value.trim());
    voidVisible.value = false;
    MessagePlugin.success(
      `${updated.invoice_no} 已作废${updated.status === 'VOID' && target.status === 'ISSUED' ? '（红字凭证已生成，应收发票号已撤）' : ''}`,
    );
    await loadInvoices(current.value);
    if (detailVisible.value && detail.value?.id === updated.id) {
      detail.value = updated;
    }
  } catch (error) {
    voidError.value = messageOf(error);
    MessagePlugin.error(voidError.value);
  } finally {
    voidSubmitting.value = false;
  }
}

onMounted(() => {
  void loadInvoices(1);
});
</script>

<style scoped>
.inv-page {
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

.tool-row__type {
  width: 140px;
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
  text-align: right;
}

.list-alert {
  margin-bottom: 12px;
}

.no-cell {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
  font-size: 12px;
  white-space: nowrap;
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

.money--tax {
  color: var(--td-text-color-secondary);
}

.text-danger {
  color: var(--td-error-color);
  font-weight: 600;
}

.op-wrap {
  display: inline-block;
}

/* ==================== 弹窗 ==================== */

.dialog-body {
  max-height: 64vh;
  overflow-y: auto;
}

.form {
  margin-top: 12px;
}

.form-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 0 16px;
}

.form-tip {
  margin: 4px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.form-tip--error {
  color: var(--td-error-color);
}

.dialog-alert {
  margin-top: 12px;
}

.issue-now {
  margin: 4px 0 12px;
}

.ar-panel {
  margin: 0 0 12px;
  padding: 10px 12px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer);
}

.ar-panel__row {
  display: flex;
  align-items: baseline;
  gap: 12px;
  padding: 2px 0;
}

.ar-panel__label {
  width: 116px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.ar-panel__value {
  font-size: 13px;
}

.void-target {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  font-size: 13px;
}

.void-impact {
  margin: 10px 0 0;
  padding: 8px 10px;
  border-left: 3px solid var(--td-warning-color);
  background: var(--td-warning-color-1, var(--td-bg-color-secondarycontainer));
  color: var(--td-text-color-primary);
  font-size: 12px;
  line-height: 1.6;
}

/* ==================== 详情抽屉 ==================== */

.detail-loading {
  padding: 24px 0;
  color: var(--td-text-color-placeholder);
  text-align: center;
}

.detail-head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.detail-head__no {
  font-size: 15px;
  font-weight: 600;
}

.detail-hint {
  margin: 10px 0 16px;
  padding: 8px 10px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer);
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.6;
}

.detail-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px 16px;
}

.detail-section {
  margin-top: 18px;
  padding-top: 12px;
  border-top: 1px solid var(--td-component-stroke);
}

.detail-section__title {
  margin-bottom: 8px;
  font-size: 13px;
  font-weight: 600;
}

.detail-item {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.detail-item__label {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.detail-item__value {
  font-size: 13px;
  word-break: break-all;
}

.detail-item--money .detail-item__value {
  font-variant-numeric: tabular-nums;
}

.detail-actions {
  display: flex;
  gap: 8px;
  margin-top: 20px;
  padding-top: 12px;
  border-top: 1px solid var(--td-component-stroke);
}
</style>
