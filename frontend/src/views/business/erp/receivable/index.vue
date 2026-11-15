<template>
  <div class="ar-page">
    <t-tabs v-model="activeTab" class="ar-tabs" @change="handleTabChange">
      <!-- ==================== ① 应收台账 ==================== -->
      <t-tab-panel value="list" label="应收台账">
        <!-- 筛选 / 分页 / 行操作 / 导出交给 ListPage（P3-06） -->
        <ListPage
          ref="listRef"
          title="应收单"
          testid="ar"
          table-testid="ar-table"
          meta-testid="ar-meta"
          btn-prefix="btn-ar"
          :columns="columns"
          :filters="filters"
          :filter-state="filter"
          :initial-page="current"
          :fetcher="fetcher"
          :query-builder="queryBuilder"
          :page-size="RECEIVABLE_PAGE_SIZE"
          exportable
          @loaded="onLoaded"
        >
          <!--
            到期日区间整块画在一个槽里，并自带 inline-flex 容器：两个日期框分属两个槽时，
            筛选行一换行就会「起（含）在上一行、止（含）在下一行」，看不出这是一个区间。
            `dueTo` 仍是声明里的筛选项（重置时由 ListPage 逐键写回空串），只是不再单独出槽。
          -->
          <template #filter-dueFrom>
            <span class="lp-date-range">
              <span class="tool-row__label">到期日</span>
              <t-date-picker
                :model-value="filter.dueFrom"
                class="tool-row__date"
                value-type="YYYY-MM-DD"
                allow-input
                clearable
                placeholder="起（含）"
                data-testid="ar-filter-due-from"
                @update:model-value="setDueFrom"
                @change="searchNow"
              />
              <span class="tool-row__tilde">~</span>
              <t-date-picker
                :model-value="filter.dueTo"
                class="tool-row__date"
                value-type="YYYY-MM-DD"
                allow-input
                clearable
                placeholder="止（含）"
                data-testid="ar-filter-due-to"
                @update:model-value="setDueTo"
                @change="searchNow"
              />
            </span>
          </template>
          <template #head-extra>
            <t-tag v-if="isFiltered" size="small" variant="light" theme="primary" data-testid="ar-filtered-tag">
              已筛选
            </t-tag>
            <span class="panel-head__tip">
              出库过账时自动挂账（来源=销售出库）；坏账核销需
              <span class="mono">business:erp:receivable:write-off</span> 权限
            </span>
          </template>
            <template #ar_no="{ row }">
              <t-link theme="primary" hover="color" :data-testid="`ar-no-${row.ar_no}`" @click="openDetail(row)">
                <span class="mono">{{ row.ar_no }}</span>
              </t-link>
            </template>

            <template #customer_name="{ row }">
              <span :class="{ muted: !row.customer_name }" :title="row.customer_name || row.customer_id">
                {{ row.customer_name || `（客户已删除 ${shortId(row.customer_id)}）` }}
              </span>
            </template>

            <template #source_no="{ row }">
              <span class="mono" :data-testid="`ar-source-${row.ar_no}`">{{ dash(row.source_no) }}</span>
              <t-tag size="small" variant="outline" class="source-tag">{{ sourceTypeLabel(row.source_type) }}</t-tag>
            </template>

            <template #amount="{ row }">
              <span class="money">{{ formatMoney(row.amount) }}</span>
            </template>

            <template #received_amount="{ row }">
              <span class="money" :class="{ muted: toCents(row.received_amount) === 0 }">
                {{ formatMoney(row.received_amount) }}
              </span>
            </template>

            <template #unsettled_amount="{ row }">
              <span class="money money--strong" :class="{ 'money--zero': toCents(row.unsettled_amount) === 0 }">
                {{ formatMoney(row.unsettled_amount) }}
              </span>
            </template>

            <template #due_date="{ row }">
              <span :data-testid="`ar-due-${row.ar_no}`">{{ dash(row.due_date) }}</span>
              <t-tag
                v-if="overdueText(row, today)"
                size="small"
                variant="light"
                :theme="overdueDaysOf(row.due_date, today) > 0 ? 'danger' : 'warning'"
                class="due-tag"
              >
                {{ overdueText(row, today) }}
              </t-tag>
            </template>

            <template #status="{ row }">
              <t-tag
                size="small"
                variant="light"
                :theme="receivableStatusTheme(row.status)"
                :title="`状态编码：${row.status}`"
                :data-testid="`ar-status-${row.ar_no}`"
              >
                {{ receivableStatusLabel(row) }}
              </t-tag>
            </template>

            <template #op="{ row }">
              <t-button size="small" variant="text" data-testid="ar-op-detail" @click="openDetail(row)">详情</t-button>
              <t-tooltip
                :content="writeOffTip(row)"
                :disabled="!writeOffTip(row)"
                placement="top-left"
                :show-arrow="true"
              >
                <span class="op-wrap">
                  <t-button
                    size="small"
                    variant="text"
                    theme="danger"
                    :disabled="!canWriteOffRow(row)"
                    :data-testid="`ar-op-writeoff-${row.ar_no}`"
                    @click="openWriteOff(row)"
                  >
                    坏账核销
                  </t-button>
                </span>
              </t-tooltip>
            </template>
        </ListPage>
      </t-tab-panel>

      <!-- ==================== ② 账龄看板 ==================== -->
      <t-tab-panel value="aging" label="账龄看板">
        <t-card :bordered="false" class="tool-card">
          <div class="tool-row">
            <span class="tool-row__label">基准日</span>
            <t-date-picker
              v-model="agingAsOf"
              class="tool-row__date"
              value-type="YYYY-MM-DD"
              allow-input
              clearable
              placeholder="留空 = 今天（CST）"
              data-testid="ar-aging-asof"
              @change="loadAging()"
            />
            <t-button theme="primary" :loading="agingLoading" data-testid="btn-ar-aging-search" @click="loadAging()">
              查询
            </t-button>
            <span class="tool-row__spacer" />
            <span class="muted">
              分档口径与应付账龄同源（后端 <span class="mono">BUCKETS</span>），前端不写死 30/60/90
            </span>
          </div>
        </t-card>

        <t-card :bordered="false" class="list-card">
          <t-alert
            v-if="agingError"
            theme="error"
            :message="agingError"
            class="list-alert"
            data-testid="ar-aging-error"
          >
            <template #operation>
              <t-button size="small" variant="outline" @click="loadAging()">重试</t-button>
            </template>
          </t-alert>

          <t-loading :loading="agingLoading" size="small">
            <div v-if="aging" class="aging-body" data-testid="ar-aging">
              <div class="aging-head">
                <h2 class="aging-head__title">{{ aging.report_name }}</h2>
                <div class="aging-head__meta">
                  <span data-testid="ar-aging-asof-text">
                    基准日：{{ aging.as_of }}（{{ agingAsOfSourceText(aging.as_of_source) }}）
                  </span>
                  <span v-if="aging.period">
                    期间：{{ aging.period_name || aging.period }}<template v-if="aging.period_status">（{{ agingPeriodStatusText(aging.period_status) }}）</template>
                  </span>
                  <span>单位：{{ aging.unit }}</span>
                </div>
              </div>

              <div class="aging-kpi" data-testid="ar-aging-kpi">
                <div class="aging-kpi__item">
                  <span class="aging-kpi__label">未收合计</span>
                  <b class="aging-kpi__value" data-testid="ar-aging-total">{{ formatMoney(aging.total_unsettled) }}</b>
                </div>
                <div class="aging-kpi__item">
                  <span class="aging-kpi__label">逾期合计</span>
                  <b class="aging-kpi__value aging-kpi__value--warn" data-testid="ar-aging-overdue">
                    {{ formatMoney(aging.overdue_unsettled) }}
                  </b>
                </div>
                <div class="aging-kpi__item">
                  <span class="aging-kpi__label">逾期占比</span>
                  <b class="aging-kpi__value" data-testid="ar-aging-ratio">{{ aging.overdue_ratio }}%</b>
                </div>
                <div class="aging-kpi__item">
                  <span class="aging-kpi__label">客户 / 单据</span>
                  <b class="aging-kpi__value">{{ aging.customer_count }} 家 / {{ aging.doc_count }} 张</b>
                </div>
              </div>

              <table class="aging-table" data-testid="ar-aging-table">
                <thead>
                  <tr>
                    <th class="aging-table__name">客户</th>
                    <th class="aging-table__num">单据数</th>
                    <th v-for="bucket in aging.buckets" :key="bucket.key" class="aging-table__num">
                      {{ bucket.label }}
                    </th>
                    <th class="aging-table__num aging-table__num--total">未收合计</th>
                    <th class="aging-table__num">最长逾期(天)</th>
                  </tr>
                </thead>
                <tbody>
                  <tr class="aging-row aging-row--total" data-testid="ar-aging-row-total">
                    <td class="aging-table__name">合计（{{ aging.customer_count }} 家）</td>
                    <td class="aging-table__num">{{ aging.doc_count }}</td>
                    <td
                      v-for="cell in aging.summary"
                      :key="cell.key"
                      class="aging-table__num"
                      :data-testid="`ar-aging-sum-${cell.key}`"
                    >
                      <span :class="{ muted: toCents(cell.unpaid) === 0 }">{{ formatMoney(cell.unpaid) }}</span>
                      <span class="aging-cell__bar">
                        <i
                          :style="{ width: `${bucketShare(cell.unpaid, aging.total_unsettled)}%`, background: bucketColor(cell.key) }"
                        />
                      </span>
                    </td>
                    <td class="aging-table__num aging-table__num--total" data-testid="ar-aging-sum-total">
                      {{ formatMoney(aging.total_unsettled) }}
                    </td>
                    <td class="aging-table__num">{{ agingMaxOverdue }}</td>
                  </tr>
                  <tr
                    v-for="(customer, index) in aging.customers"
                    :key="customer.customer_id"
                    class="aging-row"
                    :data-testid="`ar-aging-customer-${index}`"
                  >
                    <td class="aging-table__name" :data-testid="`ar-aging-customer-name-${index}`">
                      {{ customer.customer_name || '（客户已删除）' }}
                    </td>
                    <td class="aging-table__num">{{ customer.doc_count }}</td>
                    <td v-for="cell in customer.buckets" :key="cell.key" class="aging-table__num">
                      <span :class="{ muted: toCents(cell.unpaid) === 0 }">{{ formatMoney(cell.unpaid) }}</span>
                    </td>
                    <td class="aging-table__num aging-table__num--total">{{ formatMoney(customer.total_unsettled) }}</td>
                    <td class="aging-table__num">
                      <span :class="{ 'text-danger': customer.max_overdue_days > 90 }">
                        {{ customer.max_overdue_days }}
                      </span>
                    </td>
                  </tr>
                  <tr v-if="aging.customers.length === 0">
                    <td class="aging-empty" :colspan="aging.buckets.length + 4" data-testid="ar-aging-empty">
                      基准日 {{ aging.as_of }} 没有未收的应收单
                    </td>
                  </tr>
                </tbody>
              </table>

              <div v-if="aging.details.length" class="aging-detail" data-testid="ar-aging-detail">
                <div class="aging-detail__title">
                  单据明细（共 {{ aging.detail_count }} 张<template v-if="aging.detail_truncated">
                    ，本页仅列前 {{ aging.details.length }} 张</template
                  >）
                </div>
                <table class="aging-table aging-table--detail">
                  <thead>
                    <tr>
                      <th class="aging-table__name">应收单号</th>
                      <th class="aging-table__name">客户</th>
                      <th class="aging-table__name">来源单号</th>
                      <th class="aging-table__num">未收</th>
                      <th class="aging-table__num">逾期天数</th>
                      <th class="aging-table__name">档位</th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr v-for="(row, index) in aging.details" :key="row.receivable_id" class="aging-row">
                      <td class="aging-table__name">
                        <span class="mono" :data-testid="`ar-aging-detail-no-${index}`">{{ row.ar_no }}</span>
                      </td>
                      <td class="aging-table__name">{{ row.customer_name || '—' }}</td>
                      <td class="aging-table__name mono">{{ dash(row.source_no) }}</td>
                      <td class="aging-table__num">{{ formatMoney(row.unsettled_amount) }}</td>
                      <td class="aging-table__num">{{ agingDaysText(row.overdue_days) }}</td>
                      <td class="aging-table__name">
                        <t-tag
                          size="small"
                          variant="light"
                          :theme="bucketThemeOf(row.bucket)"
                          :data-testid="`ar-aging-detail-bucket-${index}`"
                        >
                          {{ row.bucket_label }}
                        </t-tag>
                      </td>
                    </tr>
                  </tbody>
                </table>
              </div>

              <p v-if="aging.hints.length" class="aging-hints">
                <span v-for="hint in aging.hints" :key="hint" class="aging-hints__item">· {{ hint }}</span>
              </p>
            </div>
            <p v-else-if="!agingLoading && !agingError" class="aging-empty" data-testid="ar-aging-blank">
              点「查询」生成账龄看板
            </p>
          </t-loading>
        </t-card>
      </t-tab-panel>
    </t-tabs>

    <!-- ==================== ③ 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      header="应收单详情"
      size="720px"
      :footer="false"
      data-testid="ar-detail-drawer"
    >
      <t-loading :loading="detailLoading" size="small">
        <t-alert v-if="detailError" theme="error" :message="detailError" data-testid="ar-detail-error">
          <template #operation>
            <t-button size="small" variant="outline" @click="reloadDetail()">重试</t-button>
          </template>
        </t-alert>

        <template v-if="detail">
          <div class="detail-head">
            <span class="detail-head__no mono" data-testid="ar-detail-no">{{ detail.ar_no }}</span>
            <t-tag
              size="small"
              variant="light"
              :theme="receivableStatusTheme(detail.status)"
              data-testid="ar-detail-status"
            >
              {{ receivableStatusLabel(detail) }}
            </t-tag>
            <t-tag v-if="overdueText(detail, today)" size="small" variant="light" theme="danger">
              {{ overdueText(detail, today) }}
            </t-tag>
          </div>

          <div class="field-grid">
            <div class="field">
              <span class="field__label">客户</span>
              <span class="field__value" data-testid="ar-detail-customer">
                {{ detail.customer_name || `（客户已删除 ${shortId(detail.customer_id)}）` }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">来源</span>
              <span class="field__value">
                {{ sourceTypeLabel(detail.source_type) }}
                <span class="mono muted">{{ dash(detail.source_no) }}</span>
              </span>
            </div>
            <div class="field">
              <span class="field__label">应收金额</span>
              <span class="field__value money" data-testid="ar-detail-amount">{{ formatMoney(detail.amount) }}</span>
            </div>
            <div class="field">
              <span class="field__label">已收金额</span>
              <span class="field__value money" data-testid="ar-detail-received">
                {{ formatMoney(detail.received_amount) }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">未收金额</span>
              <span class="field__value money money--strong" data-testid="ar-detail-unsettled">
                {{ formatMoney(detail.unsettled_amount) }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">已收比例</span>
              <span class="field__value" data-testid="ar-detail-percent">
                {{ receivedPercent(detail.amount, detail.received_amount) }}%
              </span>
            </div>
            <div class="field">
              <span class="field__label">账期（天）</span>
              <span class="field__value">{{ detail.credit_days }}<span v-if="detail.credit_days === 0" class="muted">（现结）</span></span>
            </div>
            <div class="field">
              <span class="field__label">到期日</span>
              <span class="field__value">{{ dash(detail.due_date) }}</span>
            </div>
            <div class="field">
              <span class="field__label">会计期间</span>
              <span class="field__value mono">{{ dash(detail.period_code) }}</span>
            </div>
            <div class="field">
              <span class="field__label">币种</span>
              <span class="field__value">{{ dash(detail.currency_code) }}</span>
            </div>
            <div class="field">
              <span class="field__label">发票号</span>
              <span class="field__value">{{ dash(detail.invoice_no) }}</span>
            </div>
            <div class="field">
              <span class="field__label">关联凭证</span>
              <span class="field__value mono">
                {{ detail.voucher_no || (detail.voucher_id ? shortId(detail.voucher_id) : '—') }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">结清 / 核销时间</span>
              <span class="field__value">{{ detail.settled_at ? formatDateTime(detail.settled_at) : '—' }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">备注</span>
              <span class="field__value">{{ dash(detail.remark) }}</span>
            </div>
          </div>

          <!-- ★ 核销记录：关联的收款单（SO-17 前据实空态） -->
          <div class="settle" data-testid="ar-settle">
            <div class="settle__head">
              <span class="settle__title">核销记录</span>
              <span class="settle__meta" data-testid="ar-settle-count">
                收款核销 {{ RECEIPT_ALLOCATION_READY ? '—' : '未上线' }} · 状态留痕 {{ settleRecords.length }} 条
              </span>
            </div>

            <t-timeline v-if="settleRecords.length > 0" class="settle__body">
              <t-timeline-item
                v-for="(record, index) in settleRecords"
                :key="`${record.kind}-${index}`"
                :label="record.time ? shortDate(record.time) : '—'"
                :data-testid="`ar-settle-${record.kind}-${index}`"
              >
                <div class="settle-row">
                  <t-tag size="small" variant="light" :theme="record.kind === 'WRITE_OFF' ? 'danger' : 'default'">
                    {{ record.title }}
                  </t-tag>
                  <span class="settle-row__reason">{{ record.reason }}</span>
                </div>
              </t-timeline-item>
            </t-timeline>

            <p class="settle__empty" data-testid="ar-settle-empty">
              {{ receiptAllocationEmptyText(detail.received_amount) }}
            </p>
          </div>

          <div class="detail-actions">
            <t-tooltip :content="writeOffTip(detail)" :disabled="!writeOffTip(detail)" placement="top-left">
              <span class="op-wrap">
                <t-button
                  theme="danger"
                  variant="outline"
                  :disabled="!canWriteOffRow(detail)"
                  data-testid="ar-detail-writeoff"
                  @click="openWriteOff(detail)"
                >
                  坏账核销
                </t-button>
              </span>
            </t-tooltip>
            <t-button variant="outline" @click="detailVisible = false">关闭</t-button>
          </div>
        </template>
      </t-loading>
    </t-drawer>

    <!-- ==================== ④ 坏账核销弹窗 ==================== -->
    <t-dialog
      v-model:visible="writeOffVisible"
      header="坏账核销"
      theme="warning"
      width="520px"
      :confirm-btn="{ content: '确认核销', loading: writeOffSubmitting, theme: 'danger' }"
      :close-on-overlay-click="!writeOffSubmitting"
      data-testid="ar-writeoff-dialog"
      @confirm="submitWriteOff"
    >
      <div v-if="writeOffTarget" class="writeoff">
        <t-alert theme="warning" message="坏账核销是不可逆的终态：核销后该笔应收不再计入应收资产，且不可撤销。" />

        <!-- 核销对象必须写在弹窗里：这是不可逆动作，不能在看不到「核销哪一张」的情况下点确认 -->
        <div class="writeoff__target" data-testid="ar-writeoff-target">
          <span class="mono">{{ writeOffTarget.ar_no }}</span>
          <span>{{ writeOffTarget.customer_name || '（客户已删除）' }}</span>
          <span>未收 <b class="money">{{ formatMoney(writeOffTarget.unsettled_amount) }}</b></span>
          <span class="muted">到期 {{ dash(writeOffTarget.due_date) }}</span>
        </div>

        <div class="writeoff__field">
          <span class="writeoff__label">核销原因 <i class="req">*</i></span>
          <t-textarea
            v-model="writeOffReason"
            :maxlength="WRITE_OFF_REASON_MAX"
            :autosize="{ minRows: 3, maxRows: 6 }"
            placeholder="例如：客户已注销，无法收回，经总经理批准核销"
            data-testid="ar-writeoff-reason"
          />
          <div class="writeoff__foot">
            <span class="writeoff__err" data-testid="ar-writeoff-err">{{ writeOffReasonError(writeOffReason) }}</span>
          </div>
        </div>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 应收台账（SO-15）—— 镜像应付页（payable/index.vue），账龄看板镜像应付账龄页。
 *
 * # 口径来源（逐字抄后端，不猜）
 *
 * | 语义 | 出处 |
 * |------|------|
 * | 列表行 | `receivable_query::ReceivableRes`（见 `@/api/business/erp` 的 `Receivable`） |
 * | 分页入参 | `receivable_query::ReceivablePageQuery`（`ar_no` 才是模糊键，`keyword` 会被静默忽略） |
 * | 账龄 | `receivable_aging::ReceivableAgingRes`（分档与 `payable_aging` **同源**） |
 * | 坏账核销 | `PATCH /receivable/{id}/write-off`，原因 ≥ `receivable::REASON_MIN_CHARS` |
 * | 状态域 | `OPEN / PARTIAL / SETTLED / WRITTEN_OFF / CANCELLED`（**不是**应付那套） |
 *
 * # 为什么页面不算账
 *
 * 账龄分档、汇总、逾期占比、最长逾期天数全部由后端算好，前端只按 `buckets[]` 渲染表头
 * —— 后端加档位（例如 180+）这一页自动跟上，不会两边各说一套。
 *
 * # 核销记录为什么按 remark 拆
 *
 * `biz_status_log` 没有对外接口，后端把核销 / 冲销原因**同时**写进单据 `remark`
 * （`[坏账核销] …` / `[出库作废冲销] …`，一行一条）。前缀是硬契约，
 * 见 `receivable-logic.ts` 的 `WRITE_OFF_REMARK_PREFIX`；改了后端必须同步那里。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { useRoute } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';

import {
  fetchCustomerPage,
  fetchReceivableAging,
  fetchReceivableDetail,
  fetchReceivablePage,
  writeOffReceivable,
  type Customer,
  type Receivable,
  type ReceivableAging,
  type ReceivableDetail,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
/**
 * P3-06：列表壳换成通用 `ListPage`（筛选 / 服务端分页 / 行操作 / 导出）。
 * 只换渲染与分页接线，`buildReceivableQuery` / `isReceivableFiltered` 的口径一行没动。
 */
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';

import { dash, formatMoney, shortDate, shortId, toCents } from '../shared/money';
import {
  RECEIVABLE_COLUMNS,
  RECEIVABLE_PAGE_SIZE,
  RECEIVABLE_PERMS,
  RECEIVABLE_SOURCE_OPTIONS,
  RECEIVABLE_STATUS_OPTIONS,
  RECEIPT_ALLOCATION_READY,
  WRITE_OFF_REASON_MAX,
  agingAsOfSourceText,
  agingDaysText,
  agingPeriodStatusText,
  bucketShare,
  bucketThemeOf,
  buildReceivableQuery,
  canWriteOff,
  emptyReceivableFilter,
  isReceivableFiltered,
  normalizeWriteOffReason,
  overdueDaysOf,
  overdueText,
  receivableStatusLabel,
  receivableStatusTheme,
  receiptAllocationEmptyText,
  receivedPercent,
  settlementRecordsOf,
  sourceTypeLabel,
  writeOffBlockedReason,
  writeOffReasonError,
  type ReceivableFilterState,
} from './receivable-logic';

const userStore = useUserStore();
const route = useRoute();

/** 主数据下拉一次拉多少（种子规模够用） */
const OPTION_LIMIT = 200;

const activeTab = ref<'list' | 'aging'>('list');

const statusOptions = RECEIVABLE_STATUS_OPTIONS.map((item) => ({ ...item }));
const sourceOptions = RECEIVABLE_SOURCE_OPTIONS.map((item) => ({ ...item }));
/** 表格列（colKey 与后端出参同名；行操作由页面 `#op` 插槽自绘） */
const columns = RECEIVABLE_COLUMNS;

// ==================== 列表 ====================

/**
 * 筛选行（`ListPage` 就地读写**同一个对象**，`buildReceivableQuery` 的闭包不用改）。
 *
 * 所有键都在 `filters` 里有声明：`ListPage` 的「重置」按声明逐键写回，
 * 日期区间不会像 P3-05 之前那样留在原地（那是个真 bug，见组件 `clearFilter`）。
 */
const filter = reactive<ReceivableFilterState>(emptyReceivableFilter());
/** 当前页数据（镜像；坏账核销后要看「刚才那行」） */
const rows = ref<Receivable[]>([]);
/** 总条数（镜像） */
const total = ref(0);
/** 当前页码（核销后回页用；每拉完一页由 `onLoaded` 同步） */
const current = ref(1);
/** ListPage 实例（核销 / 重开后重拉列表） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 是否筛选态（「已筛选」提示用） */
const isFiltered = computed(() => isReceivableFiltered(filter));

const customers = ref<Customer[]>([]);
const customerOptions = computed(() =>
  customers.value.map((item) => ({ label: `${item.name}（${item.code}）`, value: item.id })),
);

/** 今天（CST）—— 逾期判据与后端一致，一律取东八区日期，不受浏览器时区影响 */
const today = computed(() => {
  const now = new Date();
  const cst = new Date(now.getTime() + (now.getTimezoneOffset() + 480) * 60_000);
  return cst.toISOString().slice(0, 10);
});

/**
 * 筛选项：状态 / 客户 / 单号 / 来源 / 到期日区间。
 *
 * 下拉与日期改造前都是 `@change="handleSearch"`（值一变就查），这里用 `searchOnChange` 保手感；
 * 到期日的两个框走 `custom` 槽自绘，同样在 `@change` 里调 `searchNow`。
 */
const filters = computed<FilterDef[]>(() => [
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 150,
    options: statusOptions,
    searchOnChange: true,
    testid: 'ar-filter-status',
  },
  {
    key: 'customerId',
    type: 'select',
    placeholder: '全部客户',
    width: 200,
    options: customerOptions.value,
    filterable: true,
    searchOnChange: true,
    testid: 'ar-filter-customer',
  },
  {
    key: 'arNo',
    type: 'keyword',
    placeholder: '应收单号 / 来源单号（模糊）',
    width: 260,
    testid: 'ar-filter-no',
  },
  {
    key: 'sourceType',
    type: 'select',
    placeholder: '全部来源',
    width: 150,
    options: sourceOptions,
    searchOnChange: true,
    testid: 'ar-filter-source',
  },
  // 到期日区间：`custom` + `defaultValue: ''`（重置写回空串而不是 undefined，日期框才是受控的）
  { key: 'dueFrom', type: 'custom', defaultValue: '', testid: 'ar-filter-due-from' },
  { key: 'dueTo', type: 'custom', defaultValue: '', testid: 'ar-filter-due-to' },
]);

/** 拉一页（入参拼装沿用 `buildReceivableQuery`，空值口径由它自己的单测钉住） */
const fetcher: ListFetcher<Receivable> = (query) =>
  fetchReceivablePage(query as Parameters<typeof fetchReceivablePage>[0]);

/** 入参拼装 */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildReceivableQuery(state as ReceivableFilterState, page, pageSize);

/** 重拉到指定页（不传 = 当前页） */
async function reloadList(page: number = current.value): Promise<void> {
  await listRef.value?.load(page);
}

/** 日期框 / 下拉变了立刻查（回第 1 页，与改造前 `handleSearch` 同） */
/** 日期框写回（`t-date-picker` 的 `update:model-value` 可能是 `null`，一律归成空串） */
function setDueFrom(value: unknown): void {
  filter.dueFrom = value == null ? '' : String(value);
}

function setDueTo(value: unknown): void {
  filter.dueTo = value == null ? '' : String(value);
}

function searchNow(): void {
  void listRef.value?.search();
}

/** 一页拉完：同步页码 / 行 / 总数 */
function onLoaded(payload: { rows: Receivable[]; total: number; page: number }): void {
  rows.value = payload.rows;
  total.value = payload.total;
  current.value = payload.page;
}

function messageOf(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return error instanceof Error ? error.message : String(error);
}

async function loadCustomers(): Promise<void> {
  customers.value = await fetchCustomerPage({ page: 1, page_size: OPTION_LIMIT })
    .then((data) => data.list ?? [])
    .catch(() => [] as Customer[]);
}

function handleTabChange(value: string | number): void {
  if (value === 'aging' && !aging.value && !agingLoading.value) {
    void loadAging();
  }
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<ReceivableDetail | null>(null);
const detailId = ref('');

const settleRecords = computed(() => (detail.value ? settlementRecordsOf(detail.value) : []));

async function openDetail(row: Receivable): Promise<void> {
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
    detail.value = await fetchReceivableDetail(detailId.value);
  } catch (error) {
    detailError.value = messageOf(error);
  } finally {
    detailLoading.value = false;
  }
}

// ==================== 坏账核销 ====================

const writeOffVisible = ref(false);
const writeOffSubmitting = ref(false);
const writeOffTarget = ref<Receivable | null>(null);
const writeOffReason = ref('');

/** 有权限 + 状态允许 → 可点；否则置灰并给出原因 */
function canWriteOffRow(row: Pick<Receivable, 'status'>): boolean {
  return hasWriteOffPermission.value && canWriteOff(row);
}

/** 按钮置灰原因（有权限时只说状态原因；没权限时只说不给权限） */
function writeOffTip(row: Pick<Receivable, 'status' | 'status_label'>): string {
  if (!hasWriteOffPermission.value) return `缺少权限 ${RECEIVABLE_PERMS.writeOff}`;
  return writeOffBlockedReason(row);
}

const hasWriteOffPermission = computed(() => userStore.hasPermission(RECEIVABLE_PERMS.writeOff));

function openWriteOff(row: Receivable): void {
  if (!canWriteOffRow(row)) {
    MessagePlugin.warning(writeOffTip(row));
    return;
  }
  writeOffTarget.value = row;
  writeOffReason.value = '';
  writeOffVisible.value = true;
}

async function submitWriteOff(): Promise<void> {
  const target = writeOffTarget.value;
  if (!target) return;
  const error = writeOffReasonError(writeOffReason.value);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  writeOffSubmitting.value = true;
  try {
    const updated = await writeOffReceivable(target.id, normalizeWriteOffReason(writeOffReason.value));
    MessagePlugin.success(`${updated.ar_no} 已坏账核销`);
    writeOffVisible.value = false;
    writeOffTarget.value = null;
    // 详情开着就就地换新（后端返回回读后的完整详情）
    if (detailVisible.value && detailId.value === updated.id) {
      detail.value = updated;
    }
    await reloadList(current.value);
    if (aging.value) await loadAging();
  } catch (err) {
    MessagePlugin.error(messageOf(err));
  } finally {
    writeOffSubmitting.value = false;
  }
}

// ==================== 账龄看板 ====================

const aging = ref<ReceivableAging | null>(null);
const agingLoading = ref(false);
const agingError = ref('');
const agingAsOf = ref('');

const agingMaxOverdue = computed(() =>
  aging.value ? aging.value.customers.reduce((max, item) => Math.max(max, item.max_overdue_days), 0) : 0,
);

/** 档位配色（与 `bucketThemeOf` 同源，但表格里要的是 CSS 颜色值，不是 TDesign theme 名） */
const BUCKET_COLORS: Record<string, string> = {
  NOT_DUE: 'var(--td-success-color)',
  D0_30: 'var(--td-brand-color)',
  D31_60: 'var(--td-warning-color)',
  D61_90: 'var(--td-warning-color-light)',
  OVER_90: 'var(--td-error-color)',
};

function bucketColor(key: string): string {
  return BUCKET_COLORS[key] ?? 'var(--td-gray-color-5)';
}

async function loadAging(): Promise<void> {
  agingLoading.value = true;
  agingError.value = '';
  try {
    aging.value = await fetchReceivableAging(agingAsOf.value ? { as_of: agingAsOf.value } : {});
  } catch (error) {
    aging.value = null;
    agingError.value = messageOf(error);
  } finally {
    agingLoading.value = false;
  }
}

// ==================== 其它 ====================

/** 详情里的时间显示（`settled_at` 是 UTC ISO，LST 交给 Date 本地化） */
function formatDateTime(value: string | null | undefined): string {
  if (!value) return '—';
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return value;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${parsed.getFullYear()}-${pad(parsed.getMonth() + 1)}-${pad(parsed.getDate())} ${pad(parsed.getHours())}:${pad(parsed.getMinutes())}`;
}

onMounted(() => {
  void loadCustomers();
  applyArNoDeepLink();
  void reloadList(1);
});

/**
 * 深链 `?ar_no=AR-…`（SO-21）：从销售发票页点「关联应收」跳过来时直接筛出这张单。
 *
 * 只读 query、只在有值时写 `filter.arNo`：不带参数进页面时**不能**清掉用户在筛选条里
 * 手填的值（本页的筛选是页面状态，路由不是它的唯一来源）。
 */
function applyArNoDeepLink(): void {
  const raw = route.query.ar_no;
  const arNo = (typeof raw === 'string' ? raw : Array.isArray(raw) ? String(raw[0] ?? '') : '').trim();
  if (arNo) {
    filter.arNo = arNo;
    activeTab.value = 'list';
  }
}

watch(
  () => route.query.ar_no,
  (value) => {
    if (!value) return;
    applyArNoDeepLink();
    void reloadList(1);
  },
);
</script>

<style scoped>
.ar-page {
  padding: 16px;
}

.ar-tabs :deep(.t-tabs__content) {
  padding-top: 12px;
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

/* 日期区间整块（含标签与 ~）：作为一个 flex item，换行时整体走，不会被拆开 */
.lp-date-range {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  white-space: nowrap;
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
  font-weight: 400;
}

.text-danger {
  color: var(--td-error-color);
  font-weight: 600;
}

.source-tag {
  margin-left: 6px;
}

.due-tag {
  margin-left: 6px;
}

.op-wrap {
  display: inline-block;
}

/* ==================== 账龄看板 ==================== */

.aging-body {
  padding-top: 4px;
}

.aging-head {
  margin-bottom: 12px;
}

.aging-head__title {
  margin: 0 0 4px;
  font-size: 16px;
  font-weight: 600;
}

.aging-head__meta {
  display: flex;
  flex-wrap: wrap;
  gap: 16px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.aging-kpi {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
  margin-bottom: 16px;
}

.aging-kpi__item {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 12px 14px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;
}

.aging-kpi__label {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.aging-kpi__value {
  font-size: 18px;
  font-variant-numeric: tabular-nums;
}

.aging-kpi__value--warn {
  color: var(--td-error-color);
}

.aging-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.aging-table th,
.aging-table td {
  padding: 8px 10px;
  border-bottom: 1px solid var(--td-component-border);
  text-align: left;
  white-space: nowrap;
}

.aging-table th {
  color: var(--td-text-color-secondary);
  font-weight: 500;
  background: var(--td-bg-color-container-hover);
}

.aging-table__num {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.aging-table__name {
  text-align: left;
}

.aging-table__num--total {
  font-weight: 600;
}

.aging-row--total td {
  background: var(--td-bg-color-container-select);
  font-weight: 600;
}

.aging-row:hover td {
  background: var(--td-bg-color-container-hover);
}

.aging-cell__bar {
  display: block;
  height: 3px;
  margin-top: 4px;
  background: var(--td-component-border);
  border-radius: 2px;
  overflow: hidden;
}

.aging-cell__bar > i {
  display: block;
  height: 100%;
  border-radius: 2px;
}

.aging-empty {
  padding: 24px;
  color: var(--td-text-color-placeholder);
  text-align: center;
}

.aging-detail {
  margin-top: 20px;
}

.aging-detail__title {
  margin-bottom: 8px;
  font-size: 13px;
  font-weight: 600;
}

.aging-hints {
  margin: 12px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.aging-hints__item {
  display: block;
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

.settle {
  margin-bottom: 20px;
  padding-top: 16px;
  border-top: 1px solid var(--td-component-border);
}

.settle__head {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-bottom: 12px;
}

.settle__title {
  font-size: 14px;
  font-weight: 600;
}

.settle__meta {
  margin-left: auto;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.settle-row {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.settle-row__reason {
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.settle__empty {
  margin: 0;
  padding: 16px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;
}

.detail-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  padding-top: 12px;
  border-top: 1px solid var(--td-component-border);
}

/* ==================== 核销弹窗 ==================== */

.writeoff__target {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 4px 12px;
  margin-top: 12px;
  padding: 10px 12px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;
  font-size: 13px;
}

.writeoff__field {
  margin-top: 16px;
}

.writeoff__label {
  display: block;
  margin-bottom: 6px;
  font-size: 13px;
}

.req {
  color: var(--td-error-color);
  font-style: normal;
}

.writeoff__foot {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-top: 4px;
}

.writeoff__err {
  flex: 1;
  color: var(--td-error-color);
  font-size: 12px;
}
</style>
