<template>
  <div class="ledger-page">
    <t-card :bordered="false" class="list-card">
      <t-tabs v-model="activeTab" data-testid="ledger-tabs">
        <!-- ==================== Tab 1：总账（流水账） ==================== -->
        <t-tab-panel value="ledger" label="总账（流水）">
          <div class="tool-row">
            <t-button variant="outline" data-testid="btn-ledger-refresh" @click="refreshLedger">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>

            <t-select
              v-model="ledgerFilter.account_id"
              class="tool-row__account"
              :options="accountOptions"
              placeholder="选择科目（必填）"
              filterable
              clearable
              data-testid="ledger-filter-account"
              @change="handleLedgerSearch"
            />
            <t-input
              v-model="ledgerFilter.period_from"
              class="tool-row__period"
              placeholder="起始 yyyy-MM"
              clearable
              data-testid="ledger-filter-from"
              @enter="handleLedgerSearch"
            />
            <span class="tool-row__sep">到</span>
            <t-input
              v-model="ledgerFilter.period_to"
              class="tool-row__period"
              placeholder="结束 yyyy-MM"
              clearable
              data-testid="ledger-filter-to"
              @enter="handleLedgerSearch"
            />
            <t-button theme="primary" data-testid="btn-ledger-search" @click="handleLedgerSearch">查询</t-button>
            <t-button variant="outline" data-testid="btn-ledger-reset" @click="handleLedgerReset">重置</t-button>
          </div>

          <div class="panel-head">
            <div class="panel-head__main">
              <span class="panel-head__title">
                科目流水<span v-if="currentAccountText" class="panel-head__account" data-testid="ledger-current-account">
                  · {{ currentAccountText }}
                </span>
              </span>
              <span class="panel-head__meta" data-testid="ledger-meta">共 {{ ledgerTotal }} 条</span>
            </div>
            <!-- 逐行余额是「本页内」的净额累加（第一行含本页之前所有行），别说成「期初余额」 -->
            <span class="panel-head__tip">余额 = 本页内逐行净额累加（不含期初，期初看「科目余额表」）</span>
          </div>

          <t-alert
            v-if="ledgerError"
            theme="error"
            :message="ledgerError"
            class="list-alert"
            data-testid="ledger-load-error"
          >
            <template #operation>
              <t-button size="small" variant="outline" @click="loadLedger()">重试</t-button>
            </template>
          </t-alert>

          <t-alert
            v-if="ledgerHint"
            theme="info"
            :message="ledgerHint"
            class="list-alert"
            data-testid="ledger-hint"
          />

          <t-table
            row-key="rowKey"
            :data="ledgerRows"
            :columns="ledgerColumns"
            :loading="ledgerLoading"
            :pagination="ledgerPagination"
            hover
            data-testid="ledger-table"
            @page-change="handleLedgerPageChange"
          >
            <template #voucher_date="{ row }">
              <span class="muted">{{ shortDate(row.voucher_date) }}</span>
            </template>
            <template #voucher_no="{ row }">
              <div class="no-cell">
                <span class="mono" :data-testid="`ledger-no-${row.voucher_no}-${row.line_no}`">
                  {{ row.voucher_word }} {{ row.voucher_no }}
                </span>
                <t-tag v-if="row.is_red" size="small" variant="light" theme="danger">红字</t-tag>
              </div>
            </template>
            <template #summary="{ row }">{{ dash(row.summary) }}</template>
            <template #account="{ row }">
              <span class="account-cell">
                <span class="mono">{{ row.account_code }}</span>
                <span>{{ row.account_name }}</span>
              </span>
            </template>
            <template #debit_amount="{ row }">
              <span class="money">{{ toCents(row.debit_amount) === 0 ? '' : formatMoney(row.debit_amount) }}</span>
            </template>
            <template #credit_amount="{ row }">
              <span class="money">{{ toCents(row.credit_amount) === 0 ? '' : formatMoney(row.credit_amount) }}</span>
            </template>
            <template #running_balance="{ row }">
              <span class="money" :data-testid="`ledger-balance-${row.voucher_no}-${row.line_no}`">
                {{ formatMoney(row.running_balance) }}
              </span>
            </template>

            <template #empty>
              <div class="panel-empty" data-testid="ledger-empty">
                <t-icon name="chart-line" class="panel-empty__icon" />
                <p class="panel-empty__title">
                  {{ ledgerFilter.account_id ? '该科目本期没有发生额' : '请先选择科目' }}
                </p>
                <p class="panel-empty__desc">
                  {{
                    ledgerFilter.account_id
                      ? '换个期间再试；只有已记账（POSTED）凭证进总账'
                      : '总账是单科目的流水，先在上方下拉里选科目'
                  }}
                </p>
              </div>
            </template>
          </t-table>

          <!-- 本页合计 + ★ 余额自洽校验 -->
          <div class="footer-row" data-testid="ledger-footer">
            <span class="footer-row__label">本页合计</span>
            <span class="footer-row__money money" data-testid="ledger-footer-debit">
              {{ formatCents(ledgerTotals.debitCents) }}
            </span>
            <span class="footer-row__money money" data-testid="ledger-footer-credit">
              {{ formatCents(ledgerTotals.creditCents) }}
            </span>
            <span class="footer-row__verdict" :class="ledgerConsistent ? 'footer-row__verdict--ok' : 'footer-row__verdict--bad'">
              <t-icon :name="ledgerConsistent ? 'check-circle-filled' : 'error-circle-filled'" />
              <span data-testid="ledger-footer-verdict">
                {{ ledgerConsistent ? '逐行余额自洽' : `余额断点 ${ledgerBreakCount} 处` }}
              </span>
            </span>
          </div>
        </t-tab-panel>

        <!-- ==================== Tab 2：科目余额表 ==================== -->
        <t-tab-panel value="trial" label="科目余额表">
          <div class="tool-row">
            <t-button variant="outline" data-testid="btn-trial-refresh" @click="refreshTrial">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>

            <t-input
              v-model="trialPeriod"
              class="tool-row__period"
              placeholder="期间 yyyy-MM（必填）"
              clearable
              data-testid="trial-filter-period"
              @enter="handleTrialSearch"
            />
            <t-button theme="primary" data-testid="btn-trial-search" @click="handleTrialSearch">查询</t-button>
          </div>

          <div class="panel-head">
            <div class="panel-head__main">
              <span class="panel-head__title">科目余额表</span>
              <span class="panel-head__meta" data-testid="trial-meta">
                {{ trial ? `${trial.period} · ${trial.rows.length} 个科目` : '—' }}
              </span>
            </div>
            <span class="panel-head__tip">只算已记账（POSTED）凭证；出全部未删除科目，没发生额的是 0 行</span>
          </div>

          <t-alert
            v-if="trialError"
            theme="error"
            :message="trialError"
            class="list-alert"
            data-testid="trial-load-error"
          >
            <template #operation>
              <t-button size="small" variant="outline" @click="loadTrial()">重试</t-button>
            </template>
          </t-alert>

          <t-table
            row-key="account_id"
            :data="trialRows"
            :columns="trialColumns"
            :loading="trialLoading"
            hover
            data-testid="trial-table"
          >
            <template #account_code="{ row }">
              <span class="mono" :data-testid="`trial-code-${row.account_code}`">{{ row.account_code }}</span>
            </template>
            <template #account_name="{ row }">
              <span :data-testid="`trial-name-${row.account_code}`">{{ row.account_name }}</span>
            </template>
            <template #opening="{ row }">
              <span class="money">{{ formatCents(openingNetCentsOf(row)) }}</span>
            </template>
            <template #period_debit="{ row }">
              <span class="money" :data-testid="`trial-debit-${row.account_code}`">
                {{ toCents(row.period_debit) === 0 ? '' : formatMoney(row.period_debit) }}
              </span>
            </template>
            <template #period_credit="{ row }">
              <span class="money" :data-testid="`trial-credit-${row.account_code}`">
                {{ toCents(row.period_credit) === 0 ? '' : formatMoney(row.period_credit) }}
              </span>
            </template>
            <template #closing="{ row }">
              <span class="money" :data-testid="`trial-closing-${row.account_code}`">
                {{ formatCents(closingNetCentsOf(row)) }}
              </span>
            </template>

            <template #empty>
              <div class="panel-empty" data-testid="trial-empty">
                <t-icon name="chart-pie" class="panel-empty__icon" />
                <p class="panel-empty__title">本期没有科目数据</p>
                <p class="panel-empty__desc">确认期间是否已过账凭证</p>
              </div>
            </template>
          </t-table>

          <!-- ★★ 底部合计行：借贷合计 + 平衡校验（卡面 §五 硬要求） ★★ -->
          <div class="footer-row footer-row--totals" :class="trialBalanced ? '' : 'footer-row--bad'" data-testid="trial-footer">
            <span class="footer-row__label">合计</span>
            <span class="footer-row__money money" data-testid="trial-footer-debit">
              {{ formatCents(totals.periodDebitCents) }}
            </span>
            <span class="footer-row__money money" data-testid="trial-footer-credit">
              {{ formatCents(totals.periodCreditCents) }}
            </span>
            <span
              class="footer-row__verdict"
              :class="trialBalanced ? 'footer-row__verdict--ok' : 'footer-row__verdict--bad'"
            >
              <t-icon :name="trialBalanced ? 'check-circle-filled' : 'error-circle-filled'" />
              <span data-testid="trial-footer-verdict">{{ verdict }}</span>
            </span>
          </div>
          <p class="totals-note">
            合计口径：本期借方 Σ{{ formatCents(totals.periodDebitCents) }} / 本期贷方 Σ{{
              formatCents(totals.periodCreditCents)
            }} / 期末借 Σ{{ formatCents(totals.closingDebitCents) }} / 期末贷 Σ{{
              formatCents(totals.closingCreditCents)
            }}
          </p>
        </t-tab-panel>
      </t-tabs>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 总账页（P2-E2）—— Tab1 科目流水 + Tab2 科目余额表。
 *
 * # 字段来源（硬红线 1）
 *
 * 逐字抄 `backend/src/business/erp/ledger.rs`：
 *
 * | 接口 | 出参 | 本页用到的字段 |
 * |------|------|----------------|
 * | `GET /erp/ledger/page` | `LedgerRowRes` | `voucher_date` / `voucher_no` / `summary` / `account_code` / `debit_amount` / `credit_amount` / `running_balance` / `is_red` |
 * | `GET /erp/ledger/trial-balance` | `TrialBalanceRes` | `rows[].opening_debit|opening_credit|period_debit|period_credit|closing_debit|closing_credit` + `total_*` |
 *
 * # ★ 平衡校验必须可视化（硬红线 2）
 *
 * Tab2 底部合计行写出 Σ本期借 / Σ本期贷，并给平衡结论；`ledger-logic.ts::trialBalanceTotals`
 * 会**在前端把 rows 再加一遍**，同时逐行校验恒等式
 * `期初净额 + 本期借 − 本期贷 = 期末净额` —— 后端不平会 500，所以这层是防「汇总自洽但明细错」。
 *
 * Tab1 的 `running_balance` 是**本页内**的净额累加（不含期初），页脚给「逐行余额自洽」校验：
 * 第 i 行的余额必须等于第 i−1 行余额 + 本行净额，断了就红字报断点行数。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  fetchAccountOptions,
  fetchLedgerPage,
  fetchTrialBalance,
  type AccountRow,
  type LedgerRow,
  type TrialBalance,
  type TrialBalanceRow,
} from '@/api/business/erp';
import {
  LEDGER_PAGE_SIZE,
  accountOptionsOf,
  accountTextOf,
  buildLedgerQuery,
  defaultPeriod,
  emptyLedgerFilter,
  ledgerPageTotals,
  ledgerRangeError,
  runningBreaks,
  trialBalancePeriodError,
  trialBalanceTotals,
  trialBalanceVerdict,
  type LedgerFilterState,
} from './ledger-logic';
import { dash, formatCents, formatMoney, shortDate, toCents } from '../shared/money';

/** 当前 Tab（`ledger` = 流水，`trial` = 余额表） */
const activeTab = ref<'ledger' | 'trial'>('ledger');

/** 科目下拉一次拉多少（种子规模下够用） */
const ACCOUNT_LIMIT = 500;

/** 表格列 */
const ledgerColumns: PrimaryTableCol[] = [
  { colKey: 'voucher_date', title: '日期', width: 90 },
  { colKey: 'voucher_no', title: '凭证号', width: 220 },
  { colKey: 'summary', title: '摘要', minWidth: 160, ellipsis: true },
  { colKey: 'account', title: '科目', minWidth: 180 },
  { colKey: 'debit_amount', title: '借方', width: 120, align: 'right' },
  { colKey: 'credit_amount', title: '贷方', width: 120, align: 'right' },
  { colKey: 'running_balance', title: '余额', width: 130, align: 'right' },
];

const trialColumns: PrimaryTableCol[] = [
  { colKey: 'account_code', title: '科目编码', width: 120 },
  { colKey: 'account_name', title: '科目名称', minWidth: 180, ellipsis: true },
  { colKey: 'opening', title: '期初', width: 130, align: 'right' },
  { colKey: 'period_debit', title: '本期借方', width: 130, align: 'right' },
  { colKey: 'period_credit', title: '本期贷方', width: 130, align: 'right' },
  { colKey: 'closing', title: '期末', width: 130, align: 'right' },
];

// ==================== 科目下拉 ====================

const accounts = ref<AccountRow[]>([]);
const accountOptions = computed(() => accountOptionsOf(accounts.value));

/** 当前选中科目的展示名（流水表头回显） */
const currentAccountText = computed(() =>
  ledgerFilter.account_id ? accountTextOf(ledgerFilter.account_id, accounts.value) : '',
);

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

// ==================== Tab1：流水 ====================

const ledgerFilter = reactive<LedgerFilterState>(emptyLedgerFilter());
const ledgerRowsRaw = ref<LedgerRow[]>([]);
const ledgerTotal = ref(0);
const ledgerCurrent = ref(1);
const ledgerPageSize = LEDGER_PAGE_SIZE;
const ledgerLoading = ref(false);
const ledgerError = ref('');
/** 未选科目 / 区间非法时的提示（不发请求） */
const ledgerHint = ref('');

/** 行 key：同一张凭证可能有多行（`voucher_id` 不唯一），必须拼 `line_no` */
const ledgerRows = computed(() =>
  ledgerRowsRaw.value.map((row) => ({ ...row, rowKey: `${row.voucher_id}-${row.line_no}` })),
);

const ledgerPagination = computed(() => ({
  current: ledgerCurrent.value,
  pageSize: ledgerPageSize,
  total: ledgerTotal.value,
  showJumper: true,
  showPageSize: false,
}));

/** 本页合计（借 / 贷） */
const ledgerTotals = computed(() => ledgerPageTotals(ledgerRowsRaw.value));

/**
 * 逐行余额是否自洽。
 *
 * 第 1 行没有「上一行」可参照，`runningBreaks` 用第一行反推基准后再校验后续行 ——
 * 所以这个判定覆盖的是「本页内部的累加是否连贯」，跨页的连续性由后端的前缀和保证
 * （D1 的两次查询 + Rust 累加，单测钉住第 2/3 页）。
 */
const ledgerBreaks = computed(() => runningBreaks(ledgerRowsRaw.value));
const ledgerConsistent = computed(() => ledgerBreaks.value.length === 0);
const ledgerBreakCount = computed(() => ledgerBreaks.value.length);

/** 拉流水 */
async function loadLedger(page: number = ledgerCurrent.value): Promise<void> {
  const rangeError = ledgerRangeError(ledgerFilter);
  if (rangeError) {
    ledgerHint.value = rangeError;
    return;
  }
  const query = buildLedgerQuery(ledgerFilter, page, ledgerPageSize);
  if (!query) {
    ledgerHint.value = '请先选择科目（总账是单科目的流水）';
    ledgerRowsRaw.value = [];
    ledgerTotal.value = 0;
    return;
  }
  ledgerHint.value = '';
  ledgerLoading.value = true;
  ledgerError.value = '';
  try {
    const data = await fetchLedgerPage(query);
    ledgerRowsRaw.value = data.list ?? [];
    ledgerTotal.value = data.total ?? 0;
    const rawPage = data.page ?? page;
    const maxPage = Math.max(1, Math.ceil(ledgerTotal.value / ledgerPageSize));
    ledgerCurrent.value = Math.min(Math.max(1, rawPage), maxPage);
  } catch (error) {
    ledgerRowsRaw.value = [];
    ledgerTotal.value = 0;
    ledgerError.value = messageOf(error);
  } finally {
    ledgerLoading.value = false;
  }
}

function handleLedgerSearch(): void {
  void loadLedger(1);
}

function handleLedgerReset(): void {
  Object.assign(ledgerFilter, emptyLedgerFilter());
  ledgerRowsRaw.value = [];
  ledgerTotal.value = 0;
  ledgerHint.value = '';
  ledgerError.value = '';
}

function refreshLedger(): void {
  void loadLedger();
}

function handleLedgerPageChange(pageInfo: { current?: number }): void {
  void loadLedger(pageInfo.current ?? 1);
}

// ==================== Tab2：科目余额表 ====================

const trialPeriod = ref(defaultPeriod());
const trial = ref<TrialBalance | null>(null);
const trialLoading = ref(false);
const trialError = ref('');

const trialRows = computed<TrialBalanceRow[]>(() => trial.value?.rows ?? []);

/** 前端自己加的合计 + 三重平衡判定 */
const totals = computed(() => trialBalanceTotals(trialRows.value));

/** 平衡结论：后端 `balanced` 与前端重算**都要**为真 */
const trialBalanced = computed(() => {
  const backendBalanced = trial.value?.balanced ?? true;
  return backendBalanced && totals.value.balanced;
});

const verdict = computed(() => {
  if (!trial.value) {
    return '—';
  }
  if (!trial.value.balanced) {
    return '后端判定不平（接口应在 500 拦掉，此为异常响应）';
  }
  return trialBalanceVerdict(totals.value);
});

/** 期初净额（借 − 贷，分） */
function openingNetCentsOf(row: TrialBalanceRow): number {
  return toCents(row.opening_debit) - toCents(row.opening_credit);
}

/** 期末净额（借 − 贷，分） */
function closingNetCentsOf(row: TrialBalanceRow): number {
  return toCents(row.closing_debit) - toCents(row.closing_credit);
}

/** 拉余额表 */
async function loadTrial(): Promise<void> {
  const error = trialBalancePeriodError(trialPeriod.value);
  if (error) {
    trialError.value = error;
    trial.value = null;
    return;
  }
  trialLoading.value = true;
  trialError.value = '';
  try {
    trial.value = await fetchTrialBalance(trialPeriod.value.trim());
  } catch (error) {
    trial.value = null;
    trialError.value = messageOf(error);
  } finally {
    trialLoading.value = false;
  }
}

function handleTrialSearch(): void {
  void loadTrial();
}

function refreshTrial(): void {
  void loadTrial();
}

onMounted(async () => {
  accounts.value = await fetchAccountOptions(ACCOUNT_LIMIT).catch(() => [] as AccountRow[]);
});

/**
 * 切到余额表时自动查一次（默认期间 = 北京时间当月）。
 *
 * 为什么要自动：Tab2 的期间框默认已经是本月，用户切过去看到的却是空表 + 要再点一次「查询」，
 * 十有八九会以为「没数据」。只在**首次**切过去且还没有数据时触发，之后完全交给「查询 / 刷新」，
 * 不会覆盖用户改过的期间。
 */
watch(activeTab, (tab) => {
  if (tab === 'trial' && trial.value === null && !trialLoading.value && !trialError.value) {
    void loadTrial();
  }
});
</script>

<style scoped lang="less">
.ledger-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.list-card {
  flex: 1 1 auto;
  min-width: 0;
}

/* ==================== 工具栏 ==================== */

.tool-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  margin: 8px 0 12px;
}

.tool-row__account {
  width: 240px;
}

.tool-row__period {
  width: 140px;
}

.tool-row__sep {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

/* ==================== 表头 ==================== */

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

.panel-head__account {
  color: var(--td-text-color-secondary);
  font-weight: 400;
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

.no-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.account-cell {
  display: inline-flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
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

/* ==================== 合计行（两个 Tab 共用外框） ==================== */

.footer-row {
  display: flex;
  align-items: center;
  gap: 16px;
  margin-top: 0;
  padding: 10px 16px;
  border: 1px solid var(--td-component-stroke);
  border-top: none;
  border-radius: 0 0 6px 6px;
  background: var(--td-bg-color-container-hover);
  font-size: 14px;
}

.footer-row__label {
  min-width: 56px;
  color: var(--td-text-color-secondary);
}

.footer-row__money {
  min-width: 120px;
  text-align: right;
  font-weight: 600;
}

.footer-row__verdict {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  margin-left: auto;
  font-weight: 600;
}

.footer-row__verdict--ok {
  color: var(--td-success-color);
}

.footer-row__verdict--bad {
  color: var(--td-error-color);
}

.footer-row--bad {
  border-color: var(--td-error-color);
  background: var(--td-error-color-light);
}

.totals-note {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}
</style>
