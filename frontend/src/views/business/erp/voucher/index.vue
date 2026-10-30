<template>
  <div class="voucher-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-voucher-refresh" @click="refresh">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-input
          v-model="filter.period"
          class="tool-row__period"
          placeholder="期间 yyyy-MM"
          clearable
          data-testid="voucher-filter-period"
          @enter="handleSearch"
        />
        <t-select
          v-model="filter.status"
          class="tool-row__status"
          :options="statusFilterOptions"
          placeholder="全部状态"
          clearable
          data-testid="voucher-filter-status"
          @change="handleSearch"
        />
        <t-select
          v-model="filter.voucher_type"
          class="tool-row__type"
          :options="typeFilterOptions"
          placeholder="全部来源类型"
          clearable
          data-testid="voucher-filter-type"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.voucher_no"
          class="tool-row__no"
          placeholder="凭证号（模糊）"
          clearable
          data-testid="voucher-filter-no"
          @enter="handleSearch"
        />
        <t-button theme="primary" data-testid="btn-voucher-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-voucher-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">记账凭证</span>
          <span class="panel-head__meta" data-testid="voucher-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__tip">凭证由过账自动生成；点行看分录与借贷平衡</span>
      </div>

      <t-alert
        v-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="voucher-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadVouchers()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="voucher-table"
        @page-change="handlePageChange"
        @row-click="handleRowClick"
      >
        <template #voucher_no="{ row }">
          <div class="no-cell">
            <span class="mono" :data-testid="`voucher-no-${row.voucher_no}`">
              {{ row.voucher_word }} {{ row.voucher_no }}
            </span>
            <!-- 红字凭证：借贷互换 + is_red=1，金额仍是正数（库 CHECK 禁负数），必须标出来 -->
            <t-tag v-if="row.is_red" size="small" variant="light" theme="danger">红字</t-tag>
          </div>
        </template>

        <template #voucher_date="{ row }">
          <span class="muted">{{ shortDate(row.voucher_date) }}</span>
        </template>

        <template #period="{ row }">
          <span class="mono muted">{{ row.period }}</span>
        </template>

        <template #summary="{ row }">
          <span :data-testid="`voucher-summary-${row.voucher_no}`">{{ dash(row.summary) }}</span>
        </template>

        <template #source_type="{ row }">
          <span class="muted">{{ sourceTypeLabel(row.source_type) }}</span>
        </template>

        <template #entry_count="{ row }">
          <span class="muted">{{ row.entry_count }} 行</span>
        </template>

        <template #total_debit="{ row }">
          <span class="money" :data-testid="`voucher-debit-${row.voucher_no}`">
            {{ formatMoney(row.total_debit) }}
          </span>
        </template>

        <template #total_credit="{ row }">
          <span class="money" :data-testid="`voucher-credit-${row.voucher_no}`">
            {{ formatMoney(row.total_credit) }}
          </span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            :theme="voucherStatusTheme(row.status)"
            :data-testid="`voucher-status-${row.voucher_no}`"
          >
            {{ voucherStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <t-link theme="primary" :data-testid="`btn-voucher-view-${row.voucher_no}`" @click.stop="openDetail(row)">
            查看
          </t-link>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="voucher-empty">
            <t-icon name="bill" class="panel-empty__icon" />
            <p class="panel-empty__title">{{ filtered ? '没有匹配的凭证' : '暂无凭证' }}</p>
            <p class="panel-empty__desc">
              {{ filtered ? '换个期间 / 状态 / 凭证号再试' : '收货单过账后会自动生成凭证（同一事务）' }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ==================== ③ 详情抽屉（分录表 + ★ 借贷平衡） ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      size="760px"
      :footer="false"
      :header="false"
      data-testid="voucher-drawer"
    >
      <div v-if="detail" class="drawer" data-testid="voucher-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title mono" data-testid="voucher-detail-title">
              {{ voucherTitleOf(detail) }}
            </span>
            <t-tag size="small" :theme="voucherStatusTheme(detail.status)" data-testid="voucher-detail-status">
              {{ voucherStatusLabel(detail.status) }}
            </t-tag>
            <t-tag v-if="detail.is_red" size="small" variant="light" theme="danger">红字凭证</t-tag>
            <t-tag size="small" variant="outline" theme="default">
              来源：{{ sourceTypeLabel(detail.source_type) }}
            </t-tag>
          </div>
          <div class="drawer-head__side">
            <t-button size="small" variant="outline" data-testid="btn-voucher-drawer-close" @click="detailVisible = false">
              关闭
            </t-button>
          </div>
        </div>

        <t-loading :loading="detailLoading" size="small">
          <div class="field-grid">
            <div class="field">
              <span class="field__label">凭证日期</span>
              <span class="field__value">{{ dash(detail.voucher_date) }}</span>
            </div>
            <div class="field">
              <span class="field__label">会计期间</span>
              <span class="field__value mono">{{ dash(detail.period) }}</span>
            </div>
            <div class="field">
              <span class="field__label">摘要</span>
              <span class="field__value">{{ dash(detail.summary) }}</span>
            </div>
            <div class="field">
              <span class="field__label">来源单据</span>
              <span class="field__value mono">{{ dash(detail.source_no) }}</span>
            </div>
            <div class="field">
              <span class="field__label">记账时间</span>
              <span class="field__value">{{ dash(detail.posted_at) }}</span>
            </div>
            <div class="field">
              <span class="field__label">分录行数</span>
              <span class="field__value">{{ detail.entries.length }} 行</span>
            </div>
          </div>

          <t-table
            class="entry-table"
            row-key="id"
            :data="detail.entries"
            :columns="entryColumns"
            size="small"
            bordered
            data-testid="voucher-entry-table"
          >
            <template #line_no="{ row }">
              <span class="mono">{{ row.line_no }}</span>
            </template>
            <template #summary="{ row }">{{ dash(row.summary) }}</template>
            <template #account="{ row }">
              <span class="account-cell" :data-testid="`voucher-entry-account-${row.line_no}`">
                <span class="mono">{{ row.account_code }}</span>
                <span>{{ row.account_name }}</span>
              </span>
            </template>
            <template #debit_amount="{ row }">
              <span class="money" :data-testid="`voucher-entry-debit-${row.line_no}`">
                {{ toCents(row.debit_amount) === 0 ? '' : formatMoney(row.debit_amount) }}
              </span>
            </template>
            <template #credit_amount="{ row }">
              <span class="money" :data-testid="`voucher-entry-credit-${row.line_no}`">
                {{ toCents(row.credit_amount) === 0 ? '' : formatMoney(row.credit_amount) }}
              </span>
            </template>
          </t-table>

          <!-- ★★ 借贷平衡可视化：合计行 + 绿勾 / 红字警告 ★★ -->
          <div class="balance" :class="balanceClass" data-testid="voucher-balance">
            <div class="balance__label">合计</div>
            <div class="balance__debit money" data-testid="voucher-balance-debit">
              {{ formatCents(balance.debitCents) }}
            </div>
            <div class="balance__credit money" data-testid="voucher-balance-credit">
              {{ formatCents(balance.creditCents) }}
            </div>
            <div class="balance__verdict" :class="balanceClass">
              <t-icon :name="balanceIcon(balance.balanced)" />
              <span data-testid="voucher-balance-text">
                {{ balanceText(balance.balanced, balance.diffCents) }}
              </span>
            </div>
          </div>

          <!-- 响应自相矛盾（前端逐行加 ≠ 后端合计）时单独报警：这比账不平更该查 -->
          <t-alert
            v-if="!balance.consistent"
            theme="warning"
            message="响应自相矛盾：分录逐行合计与后端合计不一致"
            :description="balance.mismatch"
            class="balance-alert"
            data-testid="voucher-balance-mismatch"
          />
        </t-loading>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 凭证页（P2-E2）—— 列表 + 详情抽屉（分录表 + **借贷平衡可视化**）。
 *
 * # 字段来源（硬红线 1）
 *
 * 契约在 `@/api/business/erp` 的 `VoucherRow` / `VoucherDetail`，逐字抄
 * `backend/src/business/erp/voucher_query.rs` 的 `VoucherRes` / `VoucherEntryRes` /
 * `VoucherDetailRes`（`voucher_no` / `voucher_date` / `period` / `voucher_type` /
 * `status` / `total_debit` / `total_credit` / `entries[].line_no` …）。
 *
 * # ★ 借贷平衡为什么要前端重算（卡面 §四 硬要求）
 *
 * 后端有三道闸（CHECK + 详情一致性闸 + 生成时断言），正常响应必然平。
 * 但「用户看到的平衡」必须是他看得懂的两个数字对比：合计行写出 Σ借 / Σ贷，
 * 相等打绿勾、不等红字 + 差额。所以 `voucher-logic.ts::sumEntries` 在**前端逐行加一遍**，
 * 并与后端给的 `entry_total_*` / 单头 `total_*` 三方比对（{@link balanceCheck}）：
 * 三个数不一致说明响应自相矛盾，界面单独报警（比账不平更严重）。
 *
 * # 金额
 *
 * 一律 `toCents` 换整数分再比较 / 累加（见 `../shared/money.ts` 文件头），
 * 用浮点比 `226.00000000000003 === 226` 会假报「不平」。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  fetchVoucherDetail,
  fetchVoucherPage,
  type VoucherDetail,
  type VoucherEntryLine,
  type VoucherRow,
} from '@/api/business/erp';
import {
  VOUCHER_PAGE_SIZE,
  VOUCHER_STATUS_FILTER_OPTIONS,
  VOUCHER_TYPE_FILTER_OPTIONS,
  balanceCheck,
  balanceIcon,
  balanceText,
  buildVoucherQuery,
  emptyVoucherFilter,
  isVoucherFiltered,
  sourceTypeLabel,
  voucherStatusLabel,
  voucherStatusTheme,
  voucherTitleOf,
  type VoucherFilterState,
} from './voucher-logic';
import { dash, formatCents, formatMoney, shortDate, toCents } from '../shared/money';

/** 表格列（`colKey` 与 `VoucherRow` 字段同名；`source_type` / `entry_count` 不是列字段，走插槽） */
const columns: PrimaryTableCol[] = [
  { colKey: 'voucher_no', title: '凭证号', width: 230 },
  { colKey: 'voucher_date', title: '日期', width: 90 },
  { colKey: 'period', title: '期间', width: 100 },
  { colKey: 'summary', title: '摘要', minWidth: 180, ellipsis: true },
  { colKey: 'source_type', title: '来源', width: 130 },
  { colKey: 'entry_count', title: '分录', width: 80, align: 'right' },
  { colKey: 'total_debit', title: '借方', width: 130, align: 'right' },
  { colKey: 'total_credit', title: '贷方', width: 130, align: 'right' },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 80, fixed: 'right' },
];

/** 分录列（★ 借方 / 贷方两列并排，空的那侧不写 0，与纸质凭证同排版） */
const entryColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '行', width: 56, align: 'center' },
  { colKey: 'summary', title: '摘要', minWidth: 140, ellipsis: true },
  { colKey: 'account', title: '科目', minWidth: 200 },
  { colKey: 'debit_amount', title: '借方', width: 130, align: 'right' },
  { colKey: 'credit_amount', title: '贷方', width: 130, align: 'right' },
];

const statusFilterOptions = VOUCHER_STATUS_FILTER_OPTIONS.map((item) => ({ ...item }));
const typeFilterOptions = VOUCHER_TYPE_FILTER_OPTIONS.map((item) => ({ ...item }));

// ==================== 列表 ====================

const filter = reactive<VoucherFilterState>(emptyVoucherFilter());
const rows = ref<VoucherRow[]>([]);
const total = ref(0);
const current = ref(1);
const pageSize = VOUCHER_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');

const filtered = computed(() => isVoucherFiltered(filter));

const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 拉列表 */
async function loadVouchers(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchVoucherPage(buildVoucherQuery(filter, page, pageSize));
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

function handleSearch(): void {
  void loadVouchers(1);
}

function handleReset(): void {
  Object.assign(filter, emptyVoucherFilter());
  void loadVouchers(1);
}

function refresh(): void {
  void loadVouchers();
}

/** 翻页 */
function handlePageChange(pageInfo: { current?: number }): void {
  void loadVouchers(pageInfo.current ?? 1);
}

/** 行点击 → 详情（TDesign 给的是 `RowEventContext`，不是 row 本体） */
function handleRowClick(context: { row: unknown }): void {
  void openDetail(context.row as VoucherRow);
}

// ==================== 详情 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detail = ref<VoucherDetail | null>(null);

/** 空凭证（打开抽屉的占位，避免下一帧读到上一张凭证的合计） */
function emptyDetail(row: VoucherRow): VoucherDetail {
  return {
    ...row,
    entries: [] as VoucherEntryLine[],
    entry_total_debit: 0,
    entry_total_credit: 0,
    balanced: true,
  };
}

/**
 * 打开详情。
 *
 * 先用列表行占位（单头字段列表已全给），再拉详情补 `entries` ——
 * 抽屉立刻能显示凭证号 / 状态 / 来源，不会先白一下。
 */
async function openDetail(row: VoucherRow): Promise<void> {
  detail.value = emptyDetail(row);
  detailVisible.value = true;
  detailLoading.value = true;
  try {
    detail.value = await fetchVoucherDetail(row.id);
  } catch (error) {
    // 详情失败不关抽屉：单头仍可看，分录位置给出原因
    void MessagePlugin.error(messageOf(error));
  } finally {
    detailLoading.value = false;
  }
}

/** 借贷平衡三方校验（前端逐行加 / 后端分录合计 / 单头合计） */
const balance = computed(() =>
  detail.value
    ? balanceCheck(detail.value)
    : { debitCents: 0, creditCents: 0, diffCents: 0, balanced: true, consistent: true, mismatch: '' } as ReturnType<
        typeof balanceCheck
      >,
);

/** 合计行样式类（平 = 绿，不平 = 红） */
const balanceClass = computed(() => (balance.value.balanced ? 'balance--ok' : 'balance--bad'));

onMounted(() => {
  void loadVouchers(1);
});
</script>

<style scoped lang="less">
.voucher-page {
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

.tool-row__period {
  width: 130px;
}

.tool-row__status {
  width: 130px;
}

.tool-row__type {
  width: 170px;
}

.tool-row__no {
  width: 200px;
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

.no-cell {
  display: flex;
  align-items: center;
  gap: 8px;
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

.account-cell {
  display: inline-flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
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

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px 20px;
  margin-bottom: 16px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
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

.entry-table {
  margin-bottom: 0;
}

/*
 * ★ 借贷平衡合计行：与分录表的「借方 / 贷方」两列对齐（同样的右对齐宽度），
 * 让人一眼把合计和分录对上。颜色只用 TDesign 语义变量（success / danger），
 * 22 套配色下明暗都自动适配。
 */
.balance {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 10px 16px;
  border: 1px solid var(--td-component-stroke);
  border-top: none;
  border-radius: 0 0 6px 6px;
  background: var(--td-bg-color-container-hover);
  font-size: 14px;
}

.balance__label {
  min-width: 56px;
  color: var(--td-text-color-secondary);
}

.balance__debit,
.balance__credit {
  min-width: 110px;
  text-align: right;
  font-weight: 600;
}

.balance__verdict {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  margin-left: auto;
  font-weight: 600;
}

.balance--ok .balance__verdict {
  color: var(--td-success-color);
}

.balance--bad {
  border-color: var(--td-error-color);
  background: var(--td-error-color-light);
}

.balance--bad .balance__verdict {
  color: var(--td-error-color);
}

.balance-alert {
  margin-top: 12px;
}
</style>
