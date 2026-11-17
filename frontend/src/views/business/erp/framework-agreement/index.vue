<template>
  <div class="fwa-page">
    <!-- 取数码都没有：页面能进（有页面显形码）但两个 Tab 的接口全 403 —— 说清缺什么 -->
    <t-alert
      v-if="!canExec"
      theme="warning"
      class="fwa-alert"
      data-testid="fwa-no-perm"
      :message="`当前账号没有框架协议的取数权限（${PERM.execution}），请让管理员在角色授权里勾上「框架协议 · 执行量」按钮行`"
    />

    <t-tabs v-if="canExec" v-model="activeTab" data-testid="fwa-tabs">
      <!-- ==================== Tab 1：协议台账（维护视角，只读） ==================== -->
      <t-tab-panel value="ledger" label="协议台账">
        <ListPage
          ref="ledgerRef"
          title="框架协议"
          testid="fwa-ledger"
          table-testid="fwa-ledger-table"
          meta-unit="份"
          :columns="ledgerColumns"
          :filters="ledgerFilters"
          :filter-state="ledgerFilter"
          :fetcher="fetcher"
          :query-builder="ledgerQueryBuilder"
          :page-size="PAGE_SIZE"
          show-page-size
        >
          <template #op="{ row }">
            <t-space size="small">
              <t-link theme="primary" data-testid="fwa-ledger-detail" @click="openDetail(row, 'ledger')">
                详情
              </t-link>
              <t-link theme="default" data-testid="fwa-ledger-exec" @click="openDetail(row, 'execution')">
                执行量
              </t-link>
            </t-space>
          </template>

          <template #agreement_no="{ row }">
            <span class="mono">{{ row.agreement_no }}</span>
          </template>
          <template #agreement_name="{ row }">
            <div class="cell-stack">
              <span class="cell-strong">{{ row.agreement_name }}</span>
              <span class="cell-sub">
                {{ priceModeLabel(row.price_mode) }} · {{ priceTaxText(row.price_includes_tax) }} ·
                {{ row.price_currency }}
              </span>
            </div>
          </template>
          <template #supplier="{ row }">
            <div class="cell-stack">
              <span>{{ supplierText(row) }}</span>
              <span class="cell-sub">{{ ownerText(row) }}</span>
            </div>
          </template>
          <template #period="{ row }">
            <div class="cell-stack">
              <span class="mono">{{ periodText(row) }}</span>
              <span class="cell-sub">{{ validityText(row) }}</span>
            </div>
          </template>
          <template #cap_qty="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono" :class="{ 'is-unlimited': isUnlimited(row.cap_qty) }">
                {{ capText(row.cap_qty) }}
              </span>
              <span class="cell-sub">单头 {{ capText(row.total_max_qty) }}</span>
            </div>
          </template>
          <template #cap_amount="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono" :class="{ 'is-unlimited': isUnlimited(row.cap_amount) }">
                {{ capMoneyText(row.cap_amount) }}
              </span>
              <span class="cell-sub">已用 {{ moneyText(row.executed_amount_capped) }}</span>
            </div>
          </template>
          <template #item_count="{ row }">
            <div class="cell-stack">
              <span>{{ itemSummaryText(row) }}</span>
              <span class="cell-sub">{{ docCountText(row) }}</span>
            </div>
          </template>
          <template #status="{ row }">
            <div class="cell-stack">
              <t-tag :theme="statusTheme(row.status)" variant="light" size="small">
                {{ row.status_label || agreementStatusLabel(row.status) }}
              </t-tag>
              <t-tag :theme="validityTheme(row.validity_state)" variant="outline" size="small">
                {{ row.validity_state_label || validityLabel(row.validity_state) }}
              </t-tag>
            </div>
          </template>

          <template #footer>
            <div class="fwa-note" data-testid="fwa-ledger-note">
              只读台账：当前版本不支持在页面新建 / 编辑 / 提交协议（后端维护接口尚未落地，
              协议目前只能由后端导入或数据修复建立）。用量与来源单据请看「执行量」Tab。
            </div>
          </template>

          <template #empty>
            <div class="cell-sub">{{ emptyListText(ledgerFilter, 'ledger') }}</div>
          </template>
        </ListPage>
      </t-tab-panel>

      <!-- ==================== Tab 2：执行量（执行视角 + 下钻） ==================== -->
      <t-tab-panel value="execution" label="执行量">
        <ListPage
          ref="execRef"
          title="协议执行量"
          testid="fwa-exec"
          table-testid="fwa-exec-table"
          meta-unit="份"
          :columns="execColumns"
          :filters="execFilters"
          :filter-state="execFilter"
          :fetcher="fetcher"
          :query-builder="execQueryBuilder"
          :page-size="PAGE_SIZE"
          show-page-size
        >
          <template #filter-over_only="{ value, set }">
            <t-checkbox
              :model-value="value === true"
              data-testid="fwa-filter-over"
              @change="(checked) => toggleOverOnly(Boolean(checked), set)"
            >
              只看超限
            </t-checkbox>
          </template>

          <template #op="{ row }">
            <t-link theme="primary" data-testid="fwa-exec-detail" @click="openDetail(row, 'execution')">
              下钻
            </t-link>
          </template>

          <template #agreement_no="{ row }">
            <span class="mono">{{ row.agreement_no }}</span>
          </template>
          <template #agreement_name="{ row }">
            <div class="cell-stack">
              <span class="cell-strong">{{ row.agreement_name }}</span>
              <span class="cell-sub">
                {{ priceModeLabel(row.price_mode) }} · {{ priceTaxText(row.price_includes_tax) }}
              </span>
            </div>
          </template>
          <template #supplier="{ row }">
            <span>{{ supplierText(row) }}</span>
          </template>
          <template #validity="{ row }">
            <div class="cell-stack">
              <t-tag :theme="validityTheme(row.validity_state)" variant="light" size="small">
                {{ row.validity_state_label || validityLabel(row.validity_state) }}
              </t-tag>
              <span v-if="validityLeftText(row)" class="cell-sub">{{ validityLeftText(row) }}</span>
            </div>
          </template>
          <template #used="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono">{{ decimalText(row.executed_qty) }}</span>
              <span class="cell-sub">封顶 {{ capText(row.cap_qty) }}</span>
            </div>
          </template>
          <template #remaining="{ row }">
            <div class="cell-stack cell-right">
              <span class="mono" :class="{ 'is-unlimited': isUnlimited(row.remaining_qty), 'is-over': isNegative(row.remaining_qty) }">
                {{ remainingText(row.remaining_qty) }}
              </span>
              <span class="cell-sub">{{ usedAmountLabel(row.price_includes_tax) }} {{ usedAmountOf(row) }}</span>
            </div>
          </template>
          <template #exec_rate="{ row }">
            <div class="rate-cell">
              <t-progress
                :percentage="progressPercent(row.exec_qty_rate_pct)"
                theme="line"
                :status="progressStatus(row.exec_state)"
                :label="false"
              />
              <span class="rate-cell__text mono">{{ pctText(row.exec_qty_rate_pct) }}</span>
            </div>
          </template>
          <template #exec_state="{ row }">
            <div class="cell-stack">
              <t-tag :theme="execStateTheme(row.exec_state)" variant="light" size="small">
                {{ row.exec_state_label || execStateLabel(row.exec_state) }}
              </t-tag>
              <span class="cell-sub">{{ lineCountsBrief(row) }}</span>
            </div>
          </template>
          <template #docs="{ row }">
            <span class="cell-sub-inline">{{ docCountText(row) }}</span>
          </template>

          <template #empty>
            <div class="cell-sub">{{ emptyListText(execFilter, 'execution') }}</div>
          </template>
        </ListPage>
      </t-tab-panel>
    </t-tabs>

    <!-- ==================== 详情 / 下钻抽屉（两个 Tab 共用一份取数） ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      :header="drawerHeader"
      size="1080px"
      :footer="false"
      data-testid="fwa-detail-drawer"
    >
      <t-loading :loading="detailLoading" size="small">
        <t-alert v-if="detailError" theme="error" :message="detailError" data-testid="fwa-detail-error" />

        <template v-if="head">
          <!-- KPI 排：已用 / 剩余 / 执行率 / 金额 -->
          <div class="kpi-row" data-testid="fwa-kpi">
            <div class="kpi-cell">
              <span class="kpi-label">已用量</span>
              <span class="kpi-value mono">{{ decimalText(head.executed_qty) }}</span>
              <span class="kpi-sub">{{ docCountText(head) }}计入</span>
            </div>
            <div class="kpi-cell">
              <span class="kpi-label">剩余可下单</span>
              <span
                class="kpi-value mono"
                :class="{ 'is-unlimited': isUnlimited(head.remaining_qty), 'is-over': isNegative(head.remaining_qty) }"
                data-testid="fwa-kpi-remaining"
              >
                {{ remainingText(head.remaining_qty) }}
              </span>
              <span class="kpi-sub">封顶 {{ capText(head.cap_qty) }}</span>
            </div>
            <div class="kpi-cell">
              <span class="kpi-label">数量执行率</span>
              <span class="kpi-value mono" data-testid="fwa-kpi-rate">{{ pctText(head.exec_qty_rate_pct) }}</span>
              <span class="kpi-sub">{{ head.exec_state_label || execStateLabel(head.exec_state) }}</span>
            </div>
            <div class="kpi-cell">
              <span class="kpi-label">{{ usedAmountLabel(head.price_includes_tax) }}</span>
              <span class="kpi-value mono">{{ moneyText(head.executed_amount_capped) }}</span>
              <span class="kpi-sub">金额封顶 {{ capMoneyText(head.cap_amount) }}</span>
            </div>
          </div>
          <div class="kpi-foot cell-sub" data-testid="fwa-kpi-foot">
            {{ lineCountsText(head) }} · 执行率 = 已用量 ÷ 封顶（不限量时没有分母，显示「不限」）
          </div>

          <!-- 单头信息 -->
          <t-descriptions :column="3" size="small" bordered class="fwa-desc">
            <t-descriptions-item label="协议号">
              <span class="mono">{{ head.agreement_no }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="协议名称">{{ head.agreement_name }}</t-descriptions-item>
            <t-descriptions-item label="供应商">{{ supplierText(head) }}</t-descriptions-item>
            <t-descriptions-item label="经办">{{ ownerText(head) }}</t-descriptions-item>
            <t-descriptions-item label="签订日期">{{ head.sign_date || '—' }}</t-descriptions-item>
            <t-descriptions-item label="有效期">
              <div class="cell-stack">
                <span class="mono">{{ periodText(head) }}</span>
                <span class="cell-sub">{{ validityText(head) }}</span>
              </div>
            </t-descriptions-item>
            <t-descriptions-item label="协议状态">
              <t-tag :theme="statusTheme(head.status)" variant="light" size="small">
                {{ head.status_label || agreementStatusLabel(head.status) }}
              </t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="有效期状态">
              <t-tag :theme="validityTheme(head.validity_state)" variant="light" size="small">
                {{ head.validity_state_label || validityLabel(head.validity_state) }}
              </t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="今天是否生效">
              <t-tag :theme="head.is_effective ? 'success' : 'default'" variant="light" size="small">
                {{ head.is_effective ? '生效（可下单）' : '不生效（下单会被拦）' }}
              </t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="定价方式">
              {{ head.price_mode_label || priceModeLabel(head.price_mode) }}
            </t-descriptions-item>
            <t-descriptions-item label="协议价口径">{{ priceTaxText(head.price_includes_tax) }}</t-descriptions-item>
            <t-descriptions-item label="币种">{{ head.price_currency }}</t-descriptions-item>
            <t-descriptions-item label="允许高于协议价">{{ toleranceText(head.price_tolerance) }}</t-descriptions-item>
            <t-descriptions-item label="单头数量上限">{{ capText(head.total_max_qty) }}</t-descriptions-item>
            <t-descriptions-item label="单头金额上限">{{ capMoneyText(head.total_max_amount) }}</t-descriptions-item>
            <t-descriptions-item label="有效数量封顶">
              <span class="mono">{{ capText(head.cap_qty) }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="明细上限合计">
              <span class="mono">{{ capText(head.item_max_qty_sum) }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="最近下单日">{{ head.last_order_on || '未下过单' }}</t-descriptions-item>
            <t-descriptions-item label="创建时间">{{ timeText(head.created_at) }}</t-descriptions-item>
          </t-descriptions>

          <!-- 物料范围与逐行执行量 -->
          <div class="section-head">
            <span class="section-title">物料范围与执行量</span>
            <span class="cell-sub">
              封顶 <span class="mono">{{ capText(head.cap_qty) }}</span> ·
              明细全部设上限时单头封顶取明细合计，有一行不限则整体不限
            </span>
          </div>
          <t-table
            row-key="agreement_item_id"
            :data="detail?.items ?? []"
            :columns="itemColumns"
            size="small"
            data-testid="fwa-detail-items"
          >
            <template #material="{ row }">
              <div class="cell-stack">
                <span>{{ materialText(row) }}</span>
                <span class="cell-sub">{{ row.unit_name || '—' }}</span>
              </div>
            </template>
            <template #agreed_price="{ row }">
              <div class="cell-stack cell-right">
                <span class="mono">{{ decimalText(row.agreed_price) }}</span>
                <span class="cell-sub">{{ taxRateText(row.tax_rate) }}</span>
              </div>
            </template>
            <template #used_qty="{ row }">
              <div class="cell-stack cell-right">
                <span class="mono">{{ decimalText(row.executed_qty) }}</span>
                <span v-if="hasDrift(row)" class="cell-sub cell-drift" :title="driftText(row)">
                  {{ driftText(row) }}
                </span>
                <span v-else class="cell-sub">
                  上限 <span :class="{ 'is-unlimited': isUnlimited(row.max_qty) }">{{ capText(row.max_qty) }}</span>
                  · {{ row.order_doc_count }} 张单
                </span>
              </div>
            </template>
            <template #remaining_qty="{ row }">
              <span
                class="mono"
                :class="{ 'is-unlimited': isUnlimited(row.remaining_qty), 'is-over': isNegative(row.remaining_qty) }"
              >
                {{ remainingText(row.remaining_qty) }}
              </span>
            </template>
            <template #exec_rate="{ row }">
              <div class="rate-cell">
                <t-progress
                  :percentage="progressPercent(row.exec_qty_rate_pct)"
                  theme="line"
                  :status="progressStatus(row.exec_state)"
                  :label="false"
                />
                <span class="rate-cell__text mono">{{ pctText(row.exec_qty_rate_pct) }}</span>
              </div>
            </template>
            <template #exec_state="{ row }">
              <t-tag :theme="execStateTheme(row.exec_state)" variant="light" size="small">
                {{ row.exec_state_label || execStateLabel(row.exec_state) }}
              </t-tag>
            </template>
            <template #empty>
              <div class="cell-sub" data-testid="fwa-detail-items-empty">
                这份协议没有物料范围（后端 200 + 空列表，不是 404）—— 它不会产生执行量，
                也不会被下单引用。
              </div>
            </template>
          </t-table>

          <!-- 下钻：来源采购单 -->
          <div class="section-head">
            <span class="section-title">来源采购单（下钻）</span>
            <span class="cell-sub" data-testid="fwa-sources-summary">
              {{ sourcesSummaryText(detail?.sources ?? []) }} ·
              作废 / 软删的单也列在这里，但不计入已用量
            </span>
          </div>
          <t-table
            row-key="sourceKey"
            :data="sourcesRows"
            :columns="sourceColumns"
            size="small"
            data-testid="fwa-detail-sources"
          >
            <template #order_no="{ row }">
              <span class="mono">{{ sourceOrderText(row) }}</span>
            </template>
            <template #qty="{ row }">
              <span class="mono">{{ decimalText(row.qty) }}</span>
            </template>
            <template #amount="{ row }">
              <span class="mono">{{ moneyText(row.amount) }}</span>
            </template>
            <template #counted="{ row }">
              <t-tag :theme="sourceVerdictTheme(row)" variant="light" size="small">{{ row.verdict }}</t-tag>
            </template>
            <template #empty>
              <div class="cell-sub" data-testid="fwa-detail-sources-empty">
                还没有采购单引用这份协议 —— 已用量为 0 是正常的。
              </div>
            </template>
          </t-table>
        </template>
      </t-loading>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 框架协议页面（M4-11）：**协议台账** + **执行量**（两个 Tab 共用 2 条只读接口）。
 *
 * # 为什么只有两个「看」的 Tab（没有新建 / 编辑）
 *
 * 后端 `framework_agreement` 域至今只有：
 * * M4-07 表结构（`biz_framework_agreement` + `_item`）
 * * M4-08 下单引用校验（`framework_agreement_ref`，采购单在入口拦价 / 拦量）
 * * M4-09 两个只读统计接口（本页用的就是这两条）
 *
 * **没有** CRUD handler、没有 `…:create/update/delete` 权限码（V0452 只种了
 * `…:page` + `…:execution`）、`docs/planning/20261113-新卡清单.md` 的 M4 段也没有这张卡。
 * 前端照「应该有的接口」画按钮 = 用户点了必然 404（AGENTS 记录的已知坑），
 * 所以本页**只画读写得到的入口**，并在台账页脚写明当前为只读。
 *
 * # 两个 Tab 的分工
 *
 * | Tab | 关注点 | 主列 |
 * |-----|--------|------|
 * | 协议台账 | 「这份协议签了什么」——有效期 / 价格条款 / 上限 / 物料范围 | 有效期 + 状态、封顶、物料项数 |
 * | 执行量 | 「这份协议用掉多少」——已用 / 剩余 / 执行率 / 超限行 / 来源单据 | 剩余（可为负）、执行率、行状态计数 |
 *
 * # 页面**不做**的算术（全部交给后端视图，见 `framework_agreement_execution_dto.rs`）
 *
 * 1. 剩余 = 封顶 − 已用 **不由前端算**（后端给 `remaining_qty`，且**可为负**）；
 * 2. 执行率状态**不用 pct 反推**（读 `exec_state`：`OVER` 可能 pct 舍成 100.00%）；
 * 3. 有效期是否生效**不按日期自己算**（读 `is_effective` / `validity_state`，判据在库里）；
 * 4. 金额**不做跨行加总**（单头 `executed_amount_capped` 已是协议价口径的合计）。
 *
 * # 权限（V0452 的两行 auth_code，逐字一致）
 *
 * 页面显形码 `business:erp:framework-agreement:page` **只在路由 `meta.permission`**
 * （AGENTS 约定 14：后端零引用，本文件也不拿它判任何东西）；
 * 这里只用接口码 `…:execution` 判 Tab 是否可看。
 */
import { computed, ref } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher } from '@/components/ListPage/types';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import {
  fetchAgreementExecution,
  fetchExecutionPage,
  type AgreementExecRow,
  type AgreementExecution,
  type ExecSource,
  type ExecutionQuery,
} from '@/api/business/erp/framework-agreement';
import {
  PAGE_SIZE,
  STATUS_OPTIONS,
  VALIDITY_OPTIONS,
  agreementStatusLabel,
  buildExecQuery,
  buildLedgerQuery,
  capMoneyText,
  capText,
  decimalText,
  detailTitle,
  docCountText,
  driftText,
  emptyExecFilter,
  emptyLedgerFilter,
  emptyListText,
  execStateLabel,
  execStateTheme,
  hasDrift,
  itemSummaryText,
  lineCountsBrief,
  lineCountsText,
  materialText,
  moneyText,
  ownerText,
  pctText,
  periodText,
  priceModeLabel,
  priceTaxText,
  progressPercent,
  progressStatus,
  remainingText,
  sourceKey,
  sourceOrderText,
  sourcesSummaryText,
  sourceVerdictTheme,
  statusTheme,
  supplierText,
  taxRateText,
  timeText,
  toNumberOrNull,
  toleranceText,
  usedAmountLabel,
  usedAmountOf,
  validityLabel,
  validityLeftText,
  validityText,
  validityTheme,
  type ExecFilterState,
  type LedgerFilterState,
} from './framework-agreement-logic';

/** 本页用到的权限码（与 V0452 的 `sys_menu.auth_code` 逐字一致） */
const PERM = {
  /** 页面显形（**只在路由 meta 用**，本文件不拿它判断任何东西） */
  page: 'business:erp:framework-agreement:page',
  /** 两个只读接口共用 */
  execution: 'business:erp:framework-agreement:execution',
} as const;

const userStore = useUserStore();
const canExec = computed(() => userStore.hasPermission(PERM.execution));

/** 当前 Tab（只有一张码，两个 Tab 都可用，默认落在「协议台账」） */
const activeTab = ref<'ledger' | 'execution'>('ledger');

const ledgerRef = ref<InstanceType<typeof ListPage> | null>(null);
const execRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 错误文案归一（后端 400 的 message 就是给人看的，原样展示） */
function errorText(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    return error.message || fallback;
  }
  if (error instanceof Error) {
    return error.message || fallback;
  }
  return fallback;
}

/** 列表取数（两个 Tab 同一个接口，入参由各自的 queryBuilder 拼） */
const fetcher: ListFetcher<AgreementExecRow> = async (query) => {
  try {
    return await fetchExecutionPage(query as ExecutionQuery);
  } catch (error) {
    throw new Error(errorText(error, '取框架协议列表失败'));
  }
};

// ==================== Tab 1：协议台账 ====================

const ledgerFilter = ref<LedgerFilterState>(emptyLedgerFilter());

const ledgerFilters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '协议号 / 协议名称 / 供应商 / 经办人',
    width: 300,
    testid: 'fwa-filter-keyword',
  },
  { key: 'status', type: 'select', options: STATUS_OPTIONS, placeholder: '全部状态', width: 140, testid: 'fwa-filter-status' },
  {
    key: 'validity_state',
    type: 'select',
    options: VALIDITY_OPTIONS,
    placeholder: '全部有效期状态',
    width: 160,
    testid: 'fwa-filter-validity',
  },
];

const ledgerQueryBuilder = (filter: LedgerFilterState, page: number, pageSize: number) =>
  buildLedgerQuery(filter, page, pageSize);

const ledgerColumns: ColumnDef[] = [
  { colKey: 'agreement_no', title: '协议号', width: 170, fixed: 'left' },
  { colKey: 'agreement_name', title: '协议名称', minWidth: 200 },
  { colKey: 'supplier', title: '供应商 / 经办', minWidth: 170 },
  { colKey: 'period', title: '有效期', width: 200 },
  { colKey: 'cap_qty', title: '数量封顶', width: 130, align: 'right' },
  { colKey: 'cap_amount', title: '金额封顶', width: 150, align: 'right' },
  { colKey: 'item_count', title: '物料范围', width: 140 },
  { colKey: 'status', title: '状态', width: 120 },
  { colKey: 'op', title: '操作', width: 110, fixed: 'right' },
];

// ==================== Tab 2：执行量 ====================

const execFilter = ref<ExecFilterState>(emptyExecFilter());

const execFilters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '协议号 / 协议名称 / 供应商 / 经办人',
    width: 300,
    testid: 'fwa-exec-filter-keyword',
  },
  {
    key: 'validity_state',
    type: 'select',
    options: VALIDITY_OPTIONS,
    placeholder: '全部有效期状态',
    width: 160,
    searchOnChange: true,
    testid: 'fwa-exec-filter-validity',
  },
  {
    key: 'over_only',
    type: 'custom',
    defaultValue: false,
    testid: 'fwa-exec-filter-over',
  },
];

const execQueryBuilder = (filter: ExecFilterState, page: number, pageSize: number) =>
  buildExecQuery(filter, page, pageSize);

/** 「只看超限」勾选后立即查询（自定义筛选控件不会自己触发查询） */
function toggleOverOnly(checked: boolean, set: (value: unknown) => void): void {
  set(checked);
  execRef.value?.search();
}

const execColumns: ColumnDef[] = [
  { colKey: 'agreement_no', title: '协议号', width: 168, fixed: 'left' },
  { colKey: 'agreement_name', title: '协议名称', minWidth: 140 },
  { colKey: 'supplier', title: '供应商', width: 112 },
  { colKey: 'validity', title: '有效期', width: 112 },
  { colKey: 'used', title: '已用量 / 封顶', width: 112, align: 'right' },
  { colKey: 'remaining', title: '剩余', width: 124, align: 'right' },
  { colKey: 'exec_rate', title: '数量执行率', width: 132 },
  { colKey: 'exec_state', title: '行状态', width: 156 },
  { colKey: 'docs', title: '单数', width: 92 },
  { colKey: 'op', title: '操作', width: 80, fixed: 'right' },
];

// ==================== 详情抽屉 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detailMode = ref<'ledger' | 'execution'>('ledger');
/** 抽屉标题先用列表行（详情还没回来时也有标题，不会空白一瞬） */
const detailRow = ref<AgreementExecRow | null>(null);
const detail = ref<AgreementExecution | null>(null);

/** 单头（详情回来后以详情为准，回来后列表行同源） */
const head = computed<AgreementExecRow | null>(() => detail.value?.agreement ?? detailRow.value);

const drawerHeader = computed(() => {
  const title = detailTitle(head.value);
  return detailMode.value === 'execution' ? `${title} · 执行量` : title;
});

/** 来源单据表加一个稳定的行主键（同一张单多行引用同一协议行） */
const sourcesRows = computed(() =>
  (detail.value?.sources ?? []).map((source: ExecSource) => ({ ...source, sourceKey: sourceKey(source) })),
);

/** 打开抽屉并取详情（404 = 协议不存在 / 已软删，报文原样展示） */
async function openDetail(row: AgreementExecRow, mode: 'ledger' | 'execution'): Promise<void> {
  detailMode.value = mode;
  detailRow.value = row;
  detail.value = null;
  detailError.value = '';
  detailVisible.value = true;
  detailLoading.value = true;
  try {
    detail.value = await fetchAgreementExecution(row.agreement_id);
  } catch (error) {
    detailError.value = errorText(error, '取协议执行量详情失败');
  } finally {
    detailLoading.value = false;
  }
}

/** 协议行表（物料范围 + 逐行执行量，两个 Tab 共用；列宽刻意压到抽屉宽度内，免得要横向滚） */
const itemColumns: PrimaryTableCol[] = [
  { colKey: 'line_no', title: '行号', width: 60 },
  { colKey: 'material', title: '物料 / 单位', minWidth: 170 },
  { colKey: 'agreed_price', title: '协议价 / 税率', width: 120, align: 'right' },
  { colKey: 'used_qty', title: '已用量 / 上限', width: 150, align: 'right' },
  { colKey: 'remaining_qty', title: '剩余', width: 96, align: 'right' },
  { colKey: 'exec_rate', title: '执行率', width: 150 },
  { colKey: 'exec_state', title: '状态', width: 96 },
];

/** 来源采购单表 */
const sourceColumns: PrimaryTableCol[] = [
  { colKey: 'order_no', title: '采购单号', width: 160 },
  { colKey: 'order_status_label', title: '单据状态', width: 96 },
  { colKey: 'order_date', title: '下单日', width: 104 },
  { colKey: 'qty', title: '数量', width: 84, align: 'right' },
  { colKey: 'amount', title: '金额（不含税）', width: 120, align: 'right' },
  { colKey: 'counted', title: '是否计入', width: 120 },
];

// ==================== 展示小工具（模板内联太啰嗦的两个） ====================

/** 封顶 / 剩余为空 = 不限量（模板按它加「不限」的灰字样式） */
function isUnlimited(value: unknown): boolean {
  return toNumberOrNull(value as number | string | null | undefined) === null;
}

/** 负数（超上限）—— 模板按它标红，**不截断** */
function isNegative(value: unknown): boolean {
  const parsed = toNumberOrNull(value as number | string | null | undefined);
  return parsed !== null && parsed < 0;
}

</script>

<style scoped>
.fwa-page {
  padding: 16px;
}

.fwa-alert {
  margin-bottom: 12px;
}

.cell-stack {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.35;
}

.cell-right {
  align-items: flex-end;
}

.cell-strong {
  font-weight: 500;
}

.cell-sub {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.cell-drift {
  color: var(--td-warning-color);
}

.cell-sub-inline {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  white-space: nowrap;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
  font-variant-numeric: tabular-nums;
}

/* 不限量：灰字 + 斜体，与「0」在视觉上分开 */
.is-unlimited {
  color: var(--td-text-color-placeholder);
  font-style: italic;
}

/* 超上限（负数剩余）：染红 + 加粗，报表必须让 OVER 露头 */
.is-over {
  color: var(--td-error-color);
  font-weight: 600;
}

.rate-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.rate-cell__text {
  flex: 0 0 auto;
  font-size: 12px;
}

.fwa-note {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.6;
  padding-top: 8px;
}

.kpi-row {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 12px;
  margin-bottom: 8px;
}

.kpi-cell {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border-radius: 6px;
  background: var(--td-bg-color-container-hover, #f5f6f7);
}

.kpi-label {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.kpi-value {
  font-size: 18px;
  font-weight: 600;
}

.kpi-sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.kpi-foot {
  margin-bottom: 12px;
}

.fwa-desc {
  margin-bottom: 16px;
}

.section-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin: 16px 0 8px;
}

.section-title {
  font-size: 14px;
  font-weight: 600;
}
</style>
