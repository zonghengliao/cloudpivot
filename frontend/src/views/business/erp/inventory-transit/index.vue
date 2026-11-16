<template>
  <div class="transit-page">
    <t-card :bordered="false" class="transit-card">
      <!-- 主数据下拉缺读权限时给一句人话：列表本身不受影响，别让人以为「页面坏了」 -->
      <t-alert
        v-if="masterGaps.length"
        theme="info"
        class="transit-alert transit-alert--top"
        data-testid="transit-master-gap"
        :message="`以下筛选下拉不可用（缺主数据读权限，列表与查询本身不受影响）：${masterGaps.join(' / ')}`"
      />

      <t-tabs v-model="activeTab" data-testid="transit-tabs">
        <!-- ==================== Tab 1：在途明细（行级台账） ==================== -->
        <t-tab-panel :value="TRANSIT_TABS[0].key" :label="TRANSIT_TABS[0].label">
          <!--
            筛选 / 分页 / 空态 / 错误态交给 ListPage（P3-04）：
            本页只声明「列 + 筛选项 + 拼参」，并对几列做自定义渲染。
          -->
          <ListPage
            ref="detailListRef"
            title="在途明细"
            testid="transit-detail"
            table-testid="transit-detail-table"
            meta-testid="transit-detail-meta"
            btn-prefix="transit-btn"
            :columns="detailColumns"
            :filters="detailFilters"
            :filter-state="detailFilter"
            :fetcher="detailFetcher"
            :query-builder="detailQueryBuilder"
            :page-size="TRANSIT_PAGE_SIZE"
          >
            <!-- 发货日期区间：两个日期框必须画在同一个槽里，否则换行会把「起 / 止」拆两行 -->
            <template #filter-date_range>
              <t-date-range-picker
                :model-value="detailFilter.date_range"
                class="transit-tool__range"
                placeholder="发货日期区间"
                format="YYYY-MM-DD"
                clearable
                data-testid="transit-filter-date"
                @update:model-value="setDateRange"
              />
            </template>

            <template #head-extra>
              <span class="transit-tip">
                在途量 = 已发货 − 已签收（PG 生成列 <code>qty_in_transit</code>，本页只读）；
                签收在发货单详情页做，签完这一行自动清零。
                要「还有多少在路上」的合计请看「在途汇总」Tab（后端聚合，不受分页影响）。
              </span>
            </template>

            <template #ship_no="{ row }">
              <span class="mono" :data-testid="`transit-no-${row.id}`">{{ dash(row.ship_no) }}</span>
            </template>

            <template #customer="{ row }">
              <div class="cell-stack">
                <span class="cell-stack__main">{{ customerText(row) }}</span>
                <span v-if="!row.customer_id" class="cell-stack__sub">发货单已删除</span>
              </div>
            </template>

            <template #material="{ row }">
              <div class="cell-stack">
                <span class="cell-stack__main" :data-testid="`transit-material-${row.material_id}`">
                  {{ materialText(row) }}
                </span>
                <span class="cell-stack__sub">{{ dash(row.spec) }}</span>
              </div>
            </template>

            <template #warehouse="{ row }">
              <span :data-testid="`transit-warehouse-${row.warehouse_id}`">{{ warehouseText(row) }}</span>
            </template>

            <template #ship_date="{ row }">
              <span class="mono">{{ row.ship_date }}</span>
            </template>

            <template #age_days="{ row }">
              <t-tag size="small" variant="light" :theme="ageTheme(row.age_days)" :data-testid="`transit-age-${row.id}`">
                {{ ageText(row.age_days) }}
              </t-tag>
            </template>

            <template #shipped_qty="{ row }">
              <span class="num">{{ formatQty(row.shipped_qty) }}</span>
            </template>

            <template #received_qty="{ row }">
              <span class="num num--muted">{{ formatQty(row.received_qty) }}</span>
            </template>

            <template #qty_in_transit="{ row }">
              <span
                class="num num--strong"
                :class="{ 'num--zero': !isPositiveQty(row.qty_in_transit) }"
                :data-testid="`transit-qty-${row.id}`"
              >
                {{ formatQty(row.qty_in_transit) }}
              </span>
            </template>

            <template #status_label="{ row }">
              <t-tag size="small" variant="outline" :theme="statusTheme(row.status)" :data-testid="`transit-status-${row.id}`">
                {{ statusLabel(row.status, row.status_label) }}
              </t-tag>
            </template>

            <template #remark="{ row }">
              <span class="muted" :title="row.remark ?? ''">{{ dash(row.remark) }}</span>
            </template>

            <template #empty>
              <div class="transit-empty" data-testid="transit-detail-empty">
                <t-icon name="swap" class="transit-empty__icon" />
                <p class="transit-empty__title">{{ detailEmpty.title }}</p>
                <p class="transit-empty__desc">{{ detailEmpty.desc }}</p>
              </div>
            </template>
          </ListPage>
        </t-tab-panel>

        <!-- ==================== Tab 2：在途汇总（物料 × 发货仓） ==================== -->
        <t-tab-panel :value="TRANSIT_TABS[1].key" :label="TRANSIT_TABS[1].label" lazy>
          <t-card :bordered="false" class="transit-summary" data-testid="transit-summary">
            <div class="tool-row">
              <t-select
                v-model="summaryFilter.material_id"
                class="transit-tool__select"
                :options="materialOptions"
                placeholder="物料（全部）"
                filterable
                clearable
                data-testid="transit-summary-material"
              />
              <t-select
                v-model="summaryFilter.warehouse_id"
                class="transit-tool__select"
                :options="warehouseOptions"
                placeholder="发货仓库（全部）"
                filterable
                clearable
                data-testid="transit-summary-warehouse"
              />
              <t-select
                v-model="summaryFilter.customer_id"
                class="transit-tool__select"
                :options="customerOptions"
                placeholder="客户（全部）"
                filterable
                clearable
                data-testid="transit-summary-customer"
              />
              <t-button theme="primary" :loading="summaryLoading" data-testid="transit-summary-search" @click="loadSummary()">
                查询
              </t-button>
              <t-button variant="outline" data-testid="transit-summary-reset" @click="resetSummary()">重置</t-button>
              <span class="transit-tip transit-tip--inline">
                汇总恒为「未签收」；按 物料 × 发货仓 合并，行数上限 500
              </span>
            </div>

            <t-alert
              v-if="summaryError"
              theme="error"
              :message="summaryError"
              class="transit-alert"
              data-testid="transit-summary-error"
            >
              <template #operation>
                <t-button size="small" variant="outline" @click="loadSummary()">重试</t-button>
              </template>
            </t-alert>

            <t-alert
              v-if="summaryTruncatedNote"
              theme="warning"
              :message="summaryTruncatedNote"
              class="transit-alert"
              data-testid="transit-summary-truncated"
            />

            <!-- KPI：全部取后端 total_*（不受 rows 截断影响），前端不自己加 -->
            <div v-if="summary" class="transit-kpi" data-testid="transit-summary-kpi">
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">在途合计</span>
                <b class="transit-kpi__value" data-testid="transit-summary-total-qty">
                  {{ formatQty(summary.total_qty_in_transit) }}
                </b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">发货单</span>
                <b class="transit-kpi__value" data-testid="transit-summary-total-shipments">
                  {{ summary.total_shipments }} 张
                </b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">在途行</span>
                <b class="transit-kpi__value" data-testid="transit-summary-total-lines">
                  {{ summary.total_lines }} 行
                </b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">物料 × 仓库组合</span>
                <b class="transit-kpi__value" data-testid="transit-summary-row-count">
                  {{ summary.row_count }} 组
                </b>
              </div>
            </div>

            <t-loading :loading="summaryLoading" size="small">
              <t-table
                v-if="summary && summary.rows.length"
                row-key="__key"
                :data="summaryRows"
                :columns="summaryColumns"
                size="small"
                hover
                data-testid="transit-summary-table"
              >
                <template #material="{ row }">
                  <div class="cell-stack">
                    <span class="cell-stack__main" :data-testid="`transit-summary-material-${row.material_id}`">
                      {{ summaryMaterialText(row) }}
                    </span>
                    <span class="cell-stack__sub">{{ dash(row.spec) }}</span>
                  </div>
                </template>

                <template #warehouse="{ row }">
                  <span>{{ summaryWarehouseText(row) }}</span>
                </template>

                <template #qty_in_transit="{ row }">
                  <span class="num num--strong">{{ formatQty(row.qty_in_transit) }}</span>
                </template>

                <template #shipped_qty="{ row }">
                  <span class="num">{{ formatQty(row.shipped_qty) }}</span>
                </template>

                <template #received_qty="{ row }">
                  <span class="num num--muted">{{ formatQty(row.received_qty) }}</span>
                </template>

                <template #shipments="{ row }">
                  <span class="num">{{ row.shipments }}</span>
                </template>

                <template #first_ship_date="{ row }">
                  <span class="mono">{{ row.first_ship_date }}</span>
                </template>

                <template #max_age_days="{ row }">
                  <t-tag size="small" variant="light" :theme="ageTheme(row.max_age_days)">
                    {{ ageText(row.max_age_days) }}
                  </t-tag>
                </template>
              </t-table>

              <div v-else-if="!summaryLoading && !summaryError" class="transit-empty" data-testid="transit-summary-empty">
                <t-icon name="swap" class="transit-empty__icon" />
                <p class="transit-empty__title">{{ summaryEmpty.title }}</p>
                <p class="transit-empty__desc">{{ summaryEmpty.desc }}</p>
              </div>
            </t-loading>
          </t-card>
        </t-tab-panel>
      </t-tabs>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 在途库存页（M2-07）：**在途明细 + 在途汇总** 两个 Tab。
 *
 * # 数据源只有两个只读接口（都在后端 `inventory_transit` 模块，M2-04 落地）
 *
 * ```text
 * Tab1 在途明细   GET /business/erp/inventory-transit/page     TransitRow[]（分页 + 筛选）
 * Tab2 在途汇总   GET /business/erp/inventory-transit/summary  按 物料 × 发货仓（+ 全量合计）
 * ```
 *
 * 页面**不算账**：
 * * `qty_in_transit` 是 PG 生成列（`shipped_qty - received_qty`），任何写入路径都写不出「不符」的值；
 * * `age_days` / `max_age_days` 由 PG 按 `CURRENT_DATE - ship_date` 算（浏览器时区不参与）；
 * * 汇总的 `total_*` 是后端**另一条聚合查询**算的，`truncated = true` 时行不全但合计仍是全量
 *   —— 所以页面上**没有**任何 `reduce` 出来的合计（自己加一遍 = 第二套账，截断时必然算小）。
 *
 * # 两个权限码（别指望一个）
 *
 * | 码 | 挂在哪 | 管什么 |
 * |----|--------|--------|
 * | `business:erp:inventory-transit:page` | V0423 菜单页面行 + 本路由 `meta.permission` | 侧栏显形 / 前端守卫（后端零引用） |
 * | `business:erp:inventory-transit:list` | V0421 按钮行 | 两个接口的框架闸 |
 *
 * 403 时页面给出点名缺哪个码的人话（见 `requestErrorMessage`），而不是「无权限」四个字。
 *
 * # 主数据下拉是「能拉就拉」
 *
 * 物料 / 仓库 / 客户 / 发货单四个下拉各要**别的模块**的读码（`material:list` 等）。
 * 持在途码但没持这些码的角色很常见（仓管看得到在途、不一定能维护物料），所以：
 * 拉失败只让对应下拉为空 + 顶部给一句说明，**不影响列表本身**（列表的关键字/状态/日期筛选照常）。
 *
 * # 只读
 *
 * 在途行的产生（发货过账）与结转（签收确认）都在**发货单**那边（M2-02 / M2-03），
 * 本页因此没有任何写按钮 —— 在途台账是派生账，手改它等于伪造账。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';

import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import {
  fetchCustomerPage,
  fetchMaterialPage,
  fetchWarehousePage,
  type Customer,
  type MaterialRow,
  type Warehouse,
} from '@/api/business/erp';
import { fetchShipmentPage, type Shipment } from '@/api/business/erp/shipment';
import {
  fetchTransitPage,
  fetchTransitSummary,
  type TransitQuery,
  type TransitRow,
  type TransitSummary,
  type TransitSummaryRow,
} from '@/api/business/erp/inventory-transit';
import { hasPermission } from '@/utils/flow-assignee-guard';

import {
  TRANSIT_PAGE_SIZE,
  TRANSIT_STATUS_OPTIONS,
  TRANSIT_TABS,
  ageText,
  ageTheme,
  buildSummaryQuery,
  buildTransitQuery,
  customerText,
  detailEmptyText,
  emptySummaryFilter,
  emptyTransitFilter,
  formatQty,
  isPositiveQty,
  materialText,
  requestErrorMessage,
  statusLabel,
  statusTheme,
  summaryEmptyText,
  truncatedNote,
  warehouseText,
  type TransitFilterState,
  type TransitSummaryFilterState,
  type TransitTabKey,
} from './transit-logic';

/** 空值占位（null / 空串统一显示「—」，不显示 `null` 也不显示 UUID） */
function dash(value: string | null | undefined): string {
  return value === null || value === undefined || value === '' ? '—' : value;
}

// ==================== Tab ====================

const activeTab = ref<TransitTabKey>(TRANSIT_TABS[0].key);

// ==================== Tab1：在途明细 ====================

const detailListRef = ref<InstanceType<typeof ListPage> | null>(null);
const detailFilter = reactive<TransitFilterState>(emptyTransitFilter());

/** 空态文案（随筛选状态变，纯函数在 transit-logic 里被单测钉住） */
const detailEmpty = computed(() => detailEmptyText(detailFilter));

/** 明细列（colKey 与后端出参同名；自定义渲染走同名插槽） */
const detailColumns: ColumnDef[] = [
  { colKey: 'ship_no', title: '发货单号', width: 170 },
  { colKey: 'customer', title: '客户', minWidth: 180 },
  { colKey: 'material', title: '物料', minWidth: 200 },
  { colKey: 'warehouse', title: '发货仓库', width: 150 },
  { colKey: 'ship_date', title: '发货日期', width: 120 },
  { colKey: 'age_days', title: '在途天数', width: 100, align: 'center' },
  { colKey: 'shipped_qty', title: '已发货', width: 100, align: 'right' },
  { colKey: 'received_qty', title: '已签收', width: 100, align: 'right' },
  { colKey: 'qty_in_transit', title: '在途量', width: 110, align: 'right' },
  { colKey: 'status_label', title: '状态', width: 120, align: 'center' },
  { colKey: 'remark', title: '备注', minWidth: 140, ellipsis: true },
];

/**
 * 明细筛选项。
 *
 * 两个下拉**不**开 `searchOnChange`（主数据几百条，边选边查会打出一串请求），
 * 统一走「查询」按钮 —— 与即时库存页同手感。选项来自异步主数据，所以是 `computed`。
 */
const detailFilters = computed<FilterDef[]>(() => [
  {
    key: 'material_id',
    type: 'select',
    placeholder: '物料（全部）',
    width: 220,
    options: materialOptions.value,
    filterable: true,
    testid: 'transit-filter-material',
  },
  {
    key: 'warehouse_id',
    type: 'select',
    placeholder: '发货仓库（全部）',
    width: 200,
    options: warehouseOptions.value,
    filterable: true,
    testid: 'transit-filter-warehouse',
  },
  {
    key: 'customer_id',
    type: 'select',
    placeholder: '客户（全部）',
    width: 200,
    options: customerOptions.value,
    filterable: true,
    testid: 'transit-filter-customer',
  },
  {
    key: 'shipment_id',
    type: 'select',
    placeholder: '发货单号（全部）',
    width: 220,
    options: shipmentOptions.value,
    filterable: true,
    testid: 'transit-filter-shipment',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '状态（缺省=只看在途）',
    width: 190,
    options: TRANSIT_STATUS_OPTIONS.map((item) => ({ label: item.label, value: item.value })),
    testid: 'transit-filter-status',
  },
  // 发货日期区间：走 `#filter-date_range` 插槽自绘（区间是数组，不是单值）
  { key: 'date_range', type: 'custom', defaultValue: [], testid: 'transit-filter-date' },
]);

const detailFetcher: ListFetcher<TransitRow> = (query) =>
  fetchTransitPage(query as TransitQuery);

const detailQueryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildTransitQuery(state as TransitFilterState, page, pageSize) as unknown as Record<string, unknown>;

/** 一页拉完（本页不做页脚合计 —— 合计在汇总 Tab，由后端聚合，避免第二套账） */

/** 日期区间写回（`t-date-range-picker` 可能给 `null`，一律归成空数组） */
function setDateRange(value: unknown): void {
  detailFilter.date_range = Array.isArray(value) ? value.map(String) : [];
}

// ==================== Tab2：在途汇总 ====================

const summaryFilter = reactive<TransitSummaryFilterState>(emptySummaryFilter());
const summary = ref<TransitSummary | null>(null);
const summaryLoading = ref(false);
const summaryError = ref('');
const summaryLoaded = ref(false);

/** 汇总行加一个稳定的行键（物料 × 仓库 才是唯一） */
const summaryRows = computed<Array<TransitSummaryRow & { __key: string }>>(() =>
  (summary.value?.rows ?? []).map((row) => ({ ...row, __key: `${row.material_id}|${row.warehouse_id}` })),
);

/** 汇总列 */
const summaryColumns = [
  { colKey: 'material', title: '物料', minWidth: 220 },
  { colKey: 'warehouse', title: '发货仓库', width: 160 },
  { colKey: 'qty_in_transit', title: '在途量', width: 120, align: 'right' as const },
  { colKey: 'shipped_qty', title: '已发货', width: 110, align: 'right' as const },
  { colKey: 'received_qty', title: '已签收', width: 110, align: 'right' as const },
  { colKey: 'shipments', title: '发货单', width: 100, align: 'right' as const },
  { colKey: 'first_ship_date', title: '最早发货日', width: 130 },
  { colKey: 'max_age_days', title: '最长在途', width: 110, align: 'center' as const },
];

const summaryEmpty = computed(() => summaryEmptyText(summaryFilter));
const summaryTruncatedNote = computed(() => truncatedNote(summary.value));

/** 汇总物料展示（主数据软删时点名） */
function summaryMaterialText(row: TransitSummaryRow): string {
  const code = row.material_code ?? '';
  const name = row.material_name ?? '';
  if (code && name) return `${code} ${name}`;
  if (code || name) return code || name;
  return '（物料已删除）';
}

/** 汇总仓库展示 */
function summaryWarehouseText(row: TransitSummaryRow): string {
  const code = row.warehouse_code ?? '';
  const name = row.warehouse_name ?? '';
  if (code && name) return `${code} ${name}`;
  if (code || name) return code || name;
  return '（仓库已删除）';
}

/** 拉一次汇总（合计取后端 `total_*`，前端不做加法） */
async function loadSummary(): Promise<void> {
  summaryLoading.value = true;
  summaryError.value = '';
  try {
    summary.value = await fetchTransitSummary(buildSummaryQuery(summaryFilter));
    summaryLoaded.value = true;
  } catch (err) {
    summaryError.value = requestErrorMessage(err);
  } finally {
    summaryLoading.value = false;
  }
}

/** 清筛选并重拉 */
function resetSummary(): void {
  Object.assign(summaryFilter, emptySummaryFilter());
  void loadSummary();
}

/** 汇总 Tab 是 `lazy` 面板：第一次切过去才拉数据（别在首屏白打一次请求） */
watch(activeTab, (tab) => {
  if (tab === TRANSIT_TABS[1].key && !summaryLoaded.value && !summaryLoading.value) {
    void loadSummary();
  }
});

// ==================== 主数据下拉（能拉就拉） ====================

const materials = ref<MaterialRow[]>([]);
const warehouses = ref<Warehouse[]>([]);
const customers = ref<Customer[]>([]);
const shipments = ref<Shipment[]>([]);
/** 拉不到的主数据（缺权限 / 接口错）：页面给一句人话，不让下拉静默变空 */
const masterGaps = ref<string[]>([]);

const materialOptions = computed(() =>
  materials.value.map((item) => ({ label: `${item.material_code} ${item.material_name}`, value: item.id })),
);

const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({ label: `${item.warehouse_code} ${item.warehouse_name}`, value: item.id })),
);

const customerOptions = computed(() =>
  customers.value.map((item) => ({
    label: `${item.code} ${item.name}`.trim() || item.id,
    value: item.id,
  })),
);

const shipmentOptions = computed(() =>
  shipments.value.map((item) => ({
    label: item.ship_no,
    value: item.id,
  })),
);

/**
 * 拉四个主数据下拉。
 *
 * 每个都**先判权限**：缺 `*:list` 就不发请求（发了必然 403，还会在全局拦截器里弹一次错误提示）。
 * 判权用的是「能不能读这个主数据」，与在途码无关 —— 两者本来就不是一条授权线。
 */
async function loadMasterData(): Promise<void> {
  const gaps: string[] = [];
  const tasks: Array<Promise<void>> = [];

  if (hasPermission('business:erp:material:list')) {
    tasks.push(
      fetchMaterialPage({ page: 1, page_size: 200 })
        .then((res) => {
          materials.value = res.list;
        })
        .catch(() => {
          gaps.push('物料');
        }),
    );
  } else {
    gaps.push('物料');
  }

  if (hasPermission('business:erp:warehouse:list')) {
    tasks.push(
      fetchWarehousePage({ page: 1, page_size: 200 })
        .then((res) => {
          warehouses.value = res.list;
        })
        .catch(() => {
          gaps.push('仓库');
        }),
    );
  } else {
    gaps.push('仓库');
  }

  if (hasPermission('business:erp:customer:list')) {
    tasks.push(
      fetchCustomerPage({ page: 1, page_size: 200 })
        .then((res) => {
          customers.value = res.list;
        })
        .catch(() => {
          gaps.push('客户');
        }),
    );
  } else {
    gaps.push('客户');
  }

  if (hasPermission('business:erp:shipment:list')) {
    tasks.push(
      fetchShipmentPage({ status: 'POSTED', page: 1, page_size: 200 })
        .then((res) => {
          shipments.value = res.list;
        })
        .catch(() => {
          gaps.push('发货单');
        }),
    );
  } else {
    gaps.push('发货单');
  }

  await Promise.all(tasks);
  masterGaps.value = gaps;
}

onMounted(() => {
  void loadMasterData();
});
</script>

<style scoped>
.transit-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.transit-card {
  padding: 4px 4px 8px;
}

.transit-summary {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 4px;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.transit-tool__select {
  width: 200px;
}

.transit-tool__range {
  width: 260px;
}

.transit-tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.7;
}

.transit-tip--inline {
  margin-left: 4px;
}

.transit-alert {
  margin: 8px 0;
}

.transit-alert--top {
  margin: 0 0 8px;
}

.transit-kpi {
  display: flex;
  flex-wrap: wrap;
  gap: 24px;
  padding: 12px 16px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer);
}

.transit-kpi__item {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.transit-kpi__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.transit-kpi__value {
  font-size: 20px;
  font-variant-numeric: tabular-nums;
}

.cell-stack {
  display: flex;
  flex-direction: column;
  line-height: 1.5;
}

.cell-stack__main {
  word-break: break-all;
}

.cell-stack__sub {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
}

.num {
  font-variant-numeric: tabular-nums;
}

.num--strong {
  font-weight: 600;
}

.num--muted {
  color: var(--td-text-color-secondary);
}

.num--zero {
  color: var(--td-text-color-placeholder);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.transit-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  padding: 40px 0;
  color: var(--td-text-color-placeholder);
}

.transit-empty__icon {
  font-size: 28px;
}

.transit-empty__title {
  margin: 0;
  font-weight: 600;
  color: var(--td-text-color-secondary);
}

.transit-empty__desc {
  margin: 0;
  font-size: 12px;
  max-width: 560px;
  text-align: center;
  line-height: 1.8;
}
</style>
