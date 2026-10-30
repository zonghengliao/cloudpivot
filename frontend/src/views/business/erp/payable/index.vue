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
    </t-card>

    <!-- ==================== ② 列表 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">应付单</span>
          <span class="panel-head__meta" data-testid="payable-meta">共 {{ total }} 条</span>
        </div>
        <!-- ★ 本批只读：付款是第 2 批，界面上把这件事写明白，省得财务来问「为什么不能付」 -->
        <span class="panel-head__tip">本批只读：付款 / 核销在批次 2 开放</span>
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

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        hover
        data-testid="payable-table"
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
          <t-link theme="primary" :data-testid="`btn-payable-view-${row.payable_no}`" @click="openDetail(row)">
            查看
          </t-link>
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
          <div class="field-grid">
            <div class="field">
              <span class="field__label">供应商</span>
              <span class="field__value">{{ supplierNameOf(detail.supplier_id) }}</span>
            </div>
            <div class="field">
              <span class="field__label">来源单号</span>
              <span class="field__value mono">{{ dash(detail.source_no) }}</span>
            </div>
            <div class="field">
              <span class="field__label">应付金额（含税）</span>
              <span class="field__value money">{{ formatMoney(detail.amount) }}</span>
            </div>
            <div class="field">
              <span class="field__label">已付金额</span>
              <span class="field__value money">{{ formatMoney(detail.paid_amount) }}</span>
            </div>
            <div class="field">
              <span class="field__label">未付金额</span>
              <span class="field__value money">{{ formatMoney(unpaidOf(detail)) }}</span>
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

          <p class="drawer-note">本批只读：付款登记 / 核销在批次 2 开放，本页不提供任何写操作。</p>
        </div>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
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
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  fetchPayablePage,
  fetchSupplierOptions,
  type PayableRow,
  type Supplier,
} from '@/api/business/erp';
import {
  PAYABLE_PAGE_SIZE,
  PAYABLE_STATUS_FILTER_OPTIONS,
  buildPayableQuery,
  emptyPayableFilter,
  isPayableFiltered,
  overdueText,
  paidPercent,
  payableStatusLabel,
  payableStatusTheme,
  sourceText,
  supplierNameOf as supplierNameFromMap,
  unpaidOf,
  type PayableFilterState,
} from './payable-logic';
import { dash, formatMoney, shortDate, shortId } from '../shared/money';

/** 主数据下拉一次拉多少（种子规模下够用；上千条应该走远端搜索） */
const OPTION_LIMIT = 200;

/** 表格列（`colKey` 与 `PayableRow` 字段同名，插槽按 colKey 命中） */
const columns: PrimaryTableCol[] = [
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
const current = ref(1);
const pageSize = PAYABLE_PAGE_SIZE;
const loading = ref(false);
const loadError = ref('');
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
const detail = ref<PayableRow | null>(null);

/** 打开详情（列表行已经带全字段，不再多打一次接口 —— 该接口本来也还没上线） */
function openDetail(row: PayableRow): void {
  detail.value = row;
  detailVisible.value = true;
}

onMounted(() => {
  void loadBasics();
  void loadPayables(1);
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
</style>
