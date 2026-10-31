<template>
  <div class="inventory-page">
    <t-card :bordered="false" class="list-card">
      <t-tabs v-model="activeTab" data-testid="inventory-tabs">
        <!-- ==================== Tab 1：即时库存 ==================== -->
        <t-tab-panel value="stock" label="即时库存">
          <div class="tool-row">
            <t-select
              v-model="stockFilter.material_id"
              class="tool-row__select"
              :options="materialOptions"
              placeholder="物料（全部）"
              filterable
              clearable
              data-testid="stock-filter-material"
            />
            <t-select
              v-model="stockFilter.warehouse_id"
              class="tool-row__select"
              :options="warehouseOptions"
              placeholder="仓库（全部）"
              filterable
              clearable
              data-testid="stock-filter-warehouse"
            />
            <label class="tool-row__switch">
              <t-switch v-model="stockFilter.only_nonzero" size="small" data-testid="stock-switch-nonzero" />
              <span>隐藏零库存</span>
            </label>
            <t-button theme="primary" data-testid="stock-btn-search" @click="handleStockSearch">查询</t-button>
            <t-button variant="outline" data-testid="stock-btn-reset" @click="handleStockReset">重置</t-button>
            <t-button variant="outline" data-testid="stock-btn-refresh" @click="refreshStock">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>
          </div>

          <div class="panel-head">
            <div class="panel-head__main">
              <span class="panel-head__title">即时库存（物料 × 仓库）</span>
              <span class="panel-head__meta" data-testid="stock-meta">共 {{ stockTotal }} 行</span>
            </div>
            <span class="panel-head__tip">
              可用 = 数量 − 锁定；主表 <code>biz_inventory</code>，库位维度看「库位库存」Tab
            </span>
          </div>

          <t-alert
            v-if="stockError"
            theme="error"
            :message="stockError"
            class="list-alert"
            data-testid="stock-load-error"
          >
            <template #operation>
              <t-button size="small" variant="outline" @click="loadStock()">重试</t-button>
            </template>
          </t-alert>

          <t-table
            row-key="id"
            :data="stockRows"
            :columns="stockColumns"
            :loading="stockLoading"
            :pagination="stockPagination"
            hover
            data-testid="stock-table"
            @page-change="handleStockPageChange"
          >
            <template #material="{ row }">
              <div class="cell-stack">
                <span class="cell-stack__main" :data-testid="`stock-material-${row.material_id}`">
                  {{ materialCodeOf(row.material_id) }}
                </span>
                <span class="cell-stack__sub">{{ materialNameOf(row.material_id) }}</span>
              </div>
            </template>
            <template #warehouse="{ row }">
              <div class="cell-stack" :data-testid="`stock-warehouse-${row.warehouse_id}`">
                <span class="cell-stack__main mono">{{ warehouseCodeOf(row.warehouse_id) }}</span>
                <span class="cell-stack__sub">{{ warehouseNameOf(row.warehouse_id) }}</span>
              </div>
            </template>
            <template #qty="{ row }">
              <span class="num" :class="{ 'num--zero': toNumber(row.qty) === 0 }" :data-testid="`stock-qty-${row.id}`">
                {{ formatQty(row.qty) }}
              </span>
            </template>
            <template #locked_qty="{ row }">
              <span class="num" :class="{ 'num--muted': toNumber(row.locked_qty) === 0 }">
                {{ formatQty(row.locked_qty) }}
              </span>
            </template>
            <template #available="{ row }">
              <span
                class="num"
                :class="isOverLocked(row) ? 'num--bad' : ''"
                :data-testid="`stock-available-${row.id}`"
              >
                {{ formatQty(availableQty(row)) }}
              </span>
            </template>
            <template #avg_cost="{ row }">
              <span class="num num--muted">{{ formatCost(row.avg_cost) }}</span>
            </template>
            <template #amount="{ row }">
              <span class="num" :data-testid="`stock-amount-${row.id}`">{{ formatMoney(row.amount) }}</span>
            </template>
            <template #last_txn_at="{ row }">
              <span class="muted">{{ formatDateTime(row.last_txn_at) }}</span>
            </template>

            <template #empty>
              <div class="panel-empty" data-testid="stock-empty">
                <t-icon name="view-list" class="panel-empty__icon" />
                <p class="panel-empty__title">
                  {{ isStockFiltered(stockFilter) ? '当前筛选下没有库存行' : '还没有任何库存' }}
                </p>
                <p class="panel-empty__desc">
                  {{
                    isStockFiltered(stockFilter)
                      ? '换个物料 / 仓库再试；「隐藏零库存」开着时 qty=0 的行不会返回'
                      : '收货入库过账后才会写 biz_inventory（本页只读）'
                  }}
                </p>
              </div>
            </template>
          </t-table>

          <div class="footer-row" data-testid="stock-footer">
            <span class="footer-row__label">本页合计</span>
            <span class="footer-row__item">
              数量 <b class="num" data-testid="stock-footer-qty">{{ formatQty(stockTotals.qty) }}</b>
            </span>
            <span class="footer-row__item">
              锁定 <b class="num">{{ formatQty(stockTotals.locked) }}</b>
            </span>
            <span class="footer-row__item">
              可用 <b class="num" data-testid="stock-footer-available">{{ formatQty(stockTotals.available) }}</b>
            </span>
            <span class="footer-row__item">
              金额 <b class="num" data-testid="stock-footer-amount">{{ formatMoney(stockTotals.amount) }}</b>
            </span>
            <span class="footer-row__tip">合计只算本页，不是全仓库存</span>
          </div>
        </t-tab-panel>

        <!-- ==================== Tab 2：库位库存 ==================== -->
        <t-tab-panel value="location" label="库位库存">
          <div class="tool-row">
            <t-select
              v-model="locationFilter.warehouse_id"
              class="tool-row__select"
              :options="warehouseOptions"
              placeholder="仓库（全部）"
              filterable
              clearable
              data-testid="location-filter-warehouse"
            />
            <t-select
              v-model="locationFilter.material_id"
              class="tool-row__select"
              :options="materialOptions"
              placeholder="物料（全部）"
              filterable
              clearable
              data-testid="location-filter-material"
            />
            <t-select
              v-model="locationFilter.location_id"
              class="tool-row__select"
              :options="locationOptions"
              placeholder="库位（全部）"
              filterable
              clearable
              data-testid="location-filter-location"
            />
            <t-button theme="primary" data-testid="location-btn-search" @click="handleLocationSearch">查询</t-button>
            <t-button variant="outline" data-testid="location-btn-reset" @click="handleLocationReset">重置</t-button>
            <t-button variant="outline" data-testid="location-btn-refresh" @click="refreshLocation">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>
          </div>

          <div class="panel-head">
            <div class="panel-head__main">
              <span class="panel-head__title">库位库存（物料 × 仓库 × 库位）</span>
              <span class="panel-head__meta" data-testid="location-meta">共 {{ locationTotal }} 行</span>
            </div>
            <span class="panel-head__tip">
              主表 <code>biz_inventory_location</code>；库位列为空 = 该仓未分配到具体库位的数量
            </span>
          </div>

          <t-alert
            v-if="locationError"
            theme="error"
            :message="locationError"
            class="list-alert"
            data-testid="location-load-error"
          >
            <template #operation>
              <t-button size="small" variant="outline" @click="loadLocation()">重试</t-button>
            </template>
          </t-alert>

          <t-table
            row-key="id"
            :data="locationRows"
            :columns="locationColumns"
            :loading="locationLoading"
            :pagination="locationPagination"
            hover
            data-testid="location-table"
            @page-change="handleLocationPageChange"
          >
            <template #location="{ row }">
              <span v-if="row.location_id" class="cell-stack">
                <span class="cell-stack__main mono" :data-testid="`location-code-${row.id}`">
                  {{ locationCodeOf(row.location_id) }}
                </span>
                <span class="cell-stack__sub">{{ locationNameOf(row.location_id) }}</span>
              </span>
              <t-tag v-else size="small" variant="light" theme="default">未分配库位</t-tag>
            </template>
            <template #material="{ row }">
              <div class="cell-stack">
                <span class="cell-stack__main">{{ materialCodeOf(row.material_id) }}</span>
                <span class="cell-stack__sub">{{ materialNameOf(row.material_id) }}</span>
              </div>
            </template>
            <template #warehouse="{ row }">
              <div class="cell-stack">
                <span class="cell-stack__main mono">{{ warehouseCodeOf(row.warehouse_id) }}</span>
                <span class="cell-stack__sub">{{ warehouseNameOf(row.warehouse_id) }}</span>
              </div>
            </template>
            <template #qty="{ row }">
              <span class="num" :class="{ 'num--zero': toNumber(row.qty) === 0 }" :data-testid="`location-qty-${row.id}`">
                {{ formatQty(row.qty) }}
              </span>
            </template>
            <template #locked_qty="{ row }">
              <span class="num" :class="{ 'num--muted': toNumber(row.locked_qty) === 0 }">
                {{ formatQty(row.locked_qty) }}
              </span>
            </template>
            <template #last_txn_at="{ row }">
              <span class="muted">{{ formatDateTime(row.last_txn_at) }}</span>
            </template>

            <template #empty>
              <div class="panel-empty" data-testid="location-empty">
                <t-icon name="location" class="panel-empty__icon" />
                <p class="panel-empty__title">
                  {{ isLocationFiltered(locationFilter) ? '当前筛选下没有库位库存' : '还没有库位库存行' }}
                </p>
                <p class="panel-empty__desc">
                  {{
                    isLocationFiltered(locationFilter)
                      ? '换个仓库 / 物料 / 库位再试'
                      : '只在收货明细指定了库位、或调拨结算到库位时才会写 biz_inventory_location'
                  }}
                </p>
              </div>
            </template>
          </t-table>
        </t-tab-panel>

        <!-- ==================== Tab 3：库存流水 ==================== -->
        <t-tab-panel value="txn" label="库存流水">
          <div class="tool-row">
            <t-select
              v-model="txnFilter.material_id"
              class="tool-row__select"
              :options="materialOptions"
              placeholder="物料（全部）"
              filterable
              clearable
              data-testid="txn-filter-material"
            />
            <t-select
              v-model="txnFilter.warehouse_id"
              class="tool-row__select"
              :options="warehouseOptions"
              placeholder="仓库（全部）"
              filterable
              clearable
              data-testid="txn-filter-warehouse"
            />
            <t-select
              v-model="txnFilter.txn_type"
              class="tool-row__type"
              :options="txnTypeOptions"
              placeholder="类型（全部）"
              clearable
              data-testid="txn-filter-type"
            />
            <t-input
              v-model="txnFilter.doc_no"
              class="tool-row__doc"
              placeholder="单据号（精确）"
              clearable
              data-testid="txn-filter-docno"
              @enter="handleTxnSearch"
            />
            <t-date-picker
              v-model="txnFilter.date_from"
              class="tool-row__date"
              placeholder="起始日"
              format="YYYY-MM-DD"
              value-type="YYYY-MM-DD"
              clearable
              data-testid="txn-filter-from"
            />
            <span class="tool-row__sep">到</span>
            <t-date-picker
              v-model="txnFilter.date_to"
              class="tool-row__date"
              placeholder="结束日"
              format="YYYY-MM-DD"
              value-type="YYYY-MM-DD"
              clearable
              data-testid="txn-filter-to"
            />
            <t-button theme="primary" data-testid="txn-btn-search" @click="handleTxnSearch">查询</t-button>
            <t-button variant="outline" data-testid="txn-btn-reset" @click="handleTxnReset">重置</t-button>
            <t-button variant="outline" data-testid="txn-btn-refresh" @click="refreshTxn">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>
          </div>

          <div class="panel-head">
            <div class="panel-head__main">
              <span class="panel-head__title">库存流水（数量账，按记账时间倒序）</span>
              <span class="panel-head__meta" data-testid="txn-meta">共 {{ txnTotal }} 条</span>
            </div>
            <span class="panel-head__tip">
              主表 <code>biz_inventory_txn</code>；数量恒为正，方向看「类型」；点行看 before/after 快照
            </span>
          </div>

          <t-alert v-if="txnHint" theme="warning" :message="txnHint" class="list-alert" data-testid="txn-hint" />

          <t-alert v-if="txnError" theme="error" :message="txnError" class="list-alert" data-testid="txn-load-error">
            <template #operation>
              <t-button size="small" variant="outline" @click="loadTxn()">重试</t-button>
            </template>
          </t-alert>

          <t-table
            row-key="id"
            :data="txnRows"
            :columns="txnColumns"
            :loading="txnLoading"
            :pagination="txnPagination"
            hover
            data-testid="txn-table"
            @page-change="handleTxnPageChange"
            @row-click="handleTxnRowClick"
          >
            <template #created_at="{ row }">
              <span class="muted" :data-testid="`txn-time-${row.id}`">{{ formatDateTime(row.created_at) }}</span>
            </template>
            <template #txn_type="{ row }">
              <t-tag
                :theme="txnTypeTheme(row.txn_type)"
                variant="light"
                size="small"
                :data-testid="`txn-type-${row.id}`"
              >
                {{ txnTypeLabel(row.txn_type) }}
              </t-tag>
            </template>
            <template #biz_type="{ row }">
              <span :data-testid="`txn-biztype-${row.id}`" :title="row.biz_type">{{ bizTypeLabel(row.biz_type) }}</span>
            </template>
            <template #qty="{ row }">
              <span class="num" :class="row.txn_type === 'OUT' ? 'num--out' : 'num--in'">
                {{ signedQtyText(row) }}
              </span>
            </template>
            <template #unit_cost="{ row }">
              <span class="num num--muted">{{ formatCost(row.unit_cost) }}</span>
            </template>
            <template #amount="{ row }">
              <span class="num">{{ formatMoney(row.amount) }}</span>
            </template>
            <template #doc_no="{ row }">
              <div class="cell-stack">
                <span class="cell-stack__main mono" :data-testid="`txn-docno-${row.id}`">{{ row.doc_no }}</span>
                <span class="cell-stack__sub">{{ docTypeLabel(row.doc_type) }} · 行 {{ row.line_no }}</span>
              </div>
            </template>
            <template #op="{ row }">
              <t-link theme="primary" hover="color" @click.stop="openTxnDetail(row.id)">详情</t-link>
            </template>

            <template #empty>
              <div class="panel-empty" data-testid="txn-empty">
                <t-icon name="history" class="panel-empty__icon" />
                <p class="panel-empty__title">
                  {{ isTxnFiltered(txnFilter) ? '当前筛选下没有流水' : '还没有库存流水' }}
                </p>
                <p class="panel-empty__desc">
                  {{
                    isTxnFiltered(txnFilter)
                      ? '换个物料 / 仓库 / 类型 / 日期再试；单据号是精确匹配'
                      : '收货过账、调拨收发、序列号出入库都会写一条流水'
                  }}
                </p>
              </div>
            </template>
          </t-table>
        </t-tab-panel>
      </t-tabs>
    </t-card>

    <!-- ==================== 流水详情抽屉（before/after 快照对账） ==================== -->
    <t-drawer
      v-model:visible="txnDetailVisible"
      size="640px"
      :footer="false"
      :header="false"
      data-testid="txn-drawer"
      @closed="txnDetail = null"
    >
      <t-loading :loading="txnDetailLoading" size="small">
      <div v-if="txnDetail" class="drawer" data-testid="txn-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title mono">{{ txnDetail.doc_no }}</span>
            <t-tag :theme="txnTypeTheme(txnDetail.txn_type)" variant="light" size="small">
              {{ txnTypeLabel(txnDetail.txn_type) }}
            </t-tag>
            <t-tag size="small" variant="outline">{{ bizTypeLabel(txnDetail.biz_type) }}</t-tag>
          </div>
          <div class="drawer-head__side">
            <span class="muted mono">{{ txnDetail.period_code }}</span>
            <t-button size="small" variant="outline" data-testid="btn-txn-drawer-close" @click="txnDetailVisible = false">
              关闭
            </t-button>
          </div>
        </div>

        <div class="drawer-body">
          <div class="field-grid">
            <div class="field">
              <span class="field__label">物料</span>
              <span class="field__value">
                {{ materialCodeOf(txnDetail.material_id) }} · {{ materialNameOf(txnDetail.material_id) }}
              </span>
            </div>
            <div class="field">
              <span class="field__label">仓库</span>
              <span class="field__value">{{ warehouseNameOf(txnDetail.warehouse_id) }}</span>
            </div>
            <div class="field">
              <span class="field__label">单据类型</span>
              <span class="field__value">{{ docTypeLabel(txnDetail.doc_type) }}</span>
            </div>
            <div class="field">
              <span class="field__label">单据行号</span>
              <span class="field__value mono">{{ txnDetail.line_no }}</span>
            </div>
            <div class="field">
              <span class="field__label">数量</span>
              <span class="field__value num">{{ signedQtyText(txnDetail) }}</span>
            </div>
            <div class="field">
              <span class="field__label">单位成本</span>
              <span class="field__value num">{{ formatCost(txnDetail.unit_cost) }}</span>
            </div>
            <div class="field">
              <span class="field__label">金额</span>
              <span class="field__value num">{{ formatMoney(txnDetail.amount) }}</span>
            </div>
            <div class="field">
              <span class="field__label">记账时间</span>
              <span class="field__value">{{ formatDateTime(txnDetail.created_at) }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">备注</span>
              <span class="field__value">{{ dash(txnDetail.remark) }}</span>
            </div>
          </div>

          <p class="drawer-section">变动前后快照（事务内算好，用于对账）</p>
          <t-table
            row-key="k"
            :data="snapshotRows"
            :columns="snapshotColumns"
            size="small"
            :bordered="true"
            data-testid="txn-snapshot-table"
          >
            <template #name="{ row }">{{ row.name }}</template>
            <template #before="{ row }"><span class="num">{{ row.before }}</span></template>
            <template #after="{ row }"><span class="num">{{ row.after }}</span></template>
            <template #delta="{ row }">
              <span class="num" :class="row.deltaClass">{{ row.delta }}</span>
            </template>
          </t-table>

          <p
            class="drawer-note"
            :class="snapshotOk ? 'drawer-note--ok' : 'drawer-note--bad'"
            data-testid="txn-snapshot-verdict"
          >
            <t-icon :name="snapshotOk ? 'check-circle-filled' : 'error-circle-filled'" />
            {{ snapshotOk ? '快照自洽：变动前 ± 数量 = 变动后' : '快照不自洽：变动前 ± 数量 ≠ 变动后，请核对流水与库存' }}
          </p>
        </div>
      </div>
      </t-loading>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 即时库存页（P4-D）—— 一个页面三个 Tab：即时库存 / 库位库存 / 库存流水。
 *
 * # 修的是哪个 404
 *
 * `sys_menu`（V0141 …0946/0947/0948）种了**三条**菜单：`/business/erp/inventory`、
 * `/business/erp/inventory-location`、`/business/erp/inventory-txn`，但前端路由是手工注册的
 * 且一条都没注册 —— 点哪个都进 404。本页把三条路径各注册一条 route（`router/index.ts`，
 * **不用 alias**：alias 与本体共用同一条 matched record，点菜单会被判定为「同一路由」而
 * 静默跳过导航），`meta.tab` 决定进来先激活哪个 Tab（`inventory-logic.ts::tabFromMeta`），
 * 切换 Tab 时 push 对应菜单路径，避免「菜单高亮 / 地址栏 / 当前 Tab」三处打脸。
 *
 * # 字段来源（硬红线：字段名以 dto.rs 为准，数量是 `qty` 不是 `quantity`）
 *
 * | Tab | 接口 | 出参 |
 * |-----|------|------|
 * | 即时库存 | `GET /erp/inventory/page` | `dto::InventoryRes`（`qty`/`locked_qty`/`avg_cost`/`amount`/`last_txn_at`） |
 * | 库位库存 | `GET /erp/inventory-location/page` | `dto::InventoryLocationRes`（`+ location_id`，指 `biz_location.id`） |
 * | 库存流水 | `GET /erp/inventory-txn/page` | `dto::InventoryTxnRes`（`txn_type`/`biz_type`/`doc_no`/`before_*`/`after_*`） |
 *
 * # 出参只有 id，名称靠主数据本地映射
 *
 * 三个接口都只吐 `material_id` / `warehouse_id` / `location_id`，没有编码名称列。
 * 页面初始化时并行拉 物料 200 条、仓库 200 条、库位 200 条做「id → 名称」字典；
 * 查不到就显示短 id（`shortId`），**绝不显示空白**。
 *
 * # 三处口径必须写清楚（否则仓管会看错)
 *
 * 1. 「可用」是前端算的 `qty − locked_qty`，接口没有这个字段；
 * 2. 流水 `qty` 恒正，`+ / −` 是前端按 `txn_type` 加的符号；
 * 3. 页脚合计**只算本页**，不是全仓库存（全仓看「隐藏零库存」关掉后的总数与逐页合计）。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  fetchInventoryLocationPage,
  fetchInventoryPage,
  fetchInventoryTxnDetail,
  fetchInventoryTxnPage,
  fetchLocationPage,
  fetchMaterialPage,
  fetchWarehousePage,
  type InventoryLocationRow,
  type InventoryRow,
  type InventoryTxnRow,
  type Location,
  type MaterialRow,
  type Warehouse,
} from '@/api/business/erp';
import {
  INVENTORY_PAGE_SIZE,
  INVENTORY_TABS,
  TXN_TYPE_OPTIONS,
  availableQty,
  buildLocationQuery,
  buildStockQuery,
  buildTxnQuery,
  bizTypeLabel,
  clampPage,
  docTypeLabel,
  emptyLocationFilter,
  emptyStockFilter,
  emptyTxnFilter,
  isLocationFiltered,
  isOverLocked,
  isStockFiltered,
  isTxnFiltered,
  signedQtyText,
  snapshotConsistent,
  tabFromMeta,
  txnRangeError,
  txnTypeLabel,
  txnTypeTheme,
  type InventoryTabKey,
  type LocationFilterState,
  type StockFilterState,
  type TxnFilterState,
} from './inventory-logic';
import { dash, formatMoney, shortId, toNumber } from '../shared/money';

const route = useRoute();
const router = useRouter();

/** 主数据一次拉多少（种子规模下够用；拉不到只影响译名，不挡列表） */
const OPTION_LIMIT = 200;

/** 当前 Tab（`route.meta.tab` 优先，其次按路径判定，见 `tabFromMeta`） */
const activeTab = ref<InventoryTabKey>(tabFromMeta(route.meta?.tab, route.path));

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

// ==================== 主数据字典 ====================

const materials = ref<MaterialRow[]>([]);
const warehouses = ref<Warehouse[]>([]);
const locations = ref<Location[]>([]);

const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: `${item.material_code} · ${item.material_name}`,
    value: item.id,
  })),
);

const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({
    label: `${item.warehouse_code} · ${item.warehouse_name}`,
    value: item.id,
  })),
);

/** Tab2 筛选（声明在 `locationOptions` 之前：库位下拉要按所选仓库收窄） */
const locationFilter = reactive<LocationFilterState>(emptyLocationFilter());

/** 库位下拉（按当前 Tab2 所选仓库收窄；没选仓库则全量） */
const locationOptions = computed(() =>
  locations.value
    .filter((item) => !locationFilter.warehouse_id || item.warehouse_id === locationFilter.warehouse_id)
    .map((item) => ({
      label: `${item.location_code}${item.location_name ? ` · ${item.location_name}` : ''}`,
      value: item.id,
    })),
);

const materialMap = computed(() => new Map(materials.value.map((item) => [item.id, item])));
const warehouseMap = computed(() => new Map(warehouses.value.map((item) => [item.id, item])));
const locationMap = computed(() => new Map(locations.value.map((item) => [item.id, item])));

/** 物料编码（查不到显示短 id，不留白） */
function materialCodeOf(id: string): string {
  return materialMap.value.get(id)?.material_code ?? shortId(id);
}

/** 物料名称 */
function materialNameOf(id: string): string {
  return materialMap.value.get(id)?.material_name ?? '（主数据未加载）';
}

/** 仓库编码（查不到显示短 id） */
function warehouseCodeOf(id: string): string {
  return warehouseMap.value.get(id)?.warehouse_code ?? shortId(id);
}

/** 仓库名称 */
function warehouseNameOf(id: string): string {
  const hit = warehouseMap.value.get(id);
  return hit ? hit.warehouse_name : shortId(id);
}

/** 库位编码 */
function locationCodeOf(id: string | null): string {
  if (!id) {
    return '';
  }
  return locationMap.value.get(id)?.location_code ?? shortId(id);
}

/** 库位名称 */
function locationNameOf(id: string | null): string {
  if (!id) {
    return '';
  }
  return locationMap.value.get(id)?.location_name ?? '（库位主数据未加载）';
}

// ==================== 数值 / 时间格式 ====================

/** 数量：最多 4 位小数（后端 `NUMERIC(18,4)`），去掉无意义的尾零 */
function formatQty(value: number | string | null | undefined): string {
  const num = toNumber(value);
  return String(Number(num.toFixed(4)));
}

/** 均价 / 单位成本：6 位小数（`NUMERIC(18,6)`），保留 2 位展示 */
function formatCost(value: number | string | null | undefined): string {
  return formatMoney(value);
}

/** 时间：`YYYY-MM-DD HH:mm`（北京时间由浏览器本地时区渲染；后端出 RFC3339） */
function formatDateTime(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  const pad = (n: number): string => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(
    date.getMinutes(),
  )}`;
}

// ==================== Tab1：即时库存 ====================

const stockColumns: PrimaryTableCol[] = [
  { colKey: 'material', title: '物料', minWidth: 200 },
  { colKey: 'warehouse', title: '仓库', width: 160 },
  { colKey: 'qty', title: '数量', width: 110, align: 'right' },
  { colKey: 'locked_qty', title: '锁定', width: 100, align: 'right' },
  { colKey: 'available', title: '可用', width: 110, align: 'right' },
  { colKey: 'avg_cost', title: '均价', width: 110, align: 'right' },
  { colKey: 'amount', title: '金额', width: 130, align: 'right' },
  { colKey: 'last_txn_at', title: '最近流水', width: 150 },
];

const stockFilter = reactive<StockFilterState>(emptyStockFilter());
const stockRows = ref<InventoryRow[]>([]);
const stockTotal = ref(0);
const stockCurrent = ref(1);
const stockLoading = ref(false);
const stockError = ref('');

/** 本页合计（数量 / 锁定 / 可用 / 金额），只算本页 */
const stockTotals = computed(() => {
  let qty = 0;
  let locked = 0;
  let available = 0;
  let amount = 0;
  for (const row of stockRows.value) {
    qty += toNumber(row.qty);
    locked += toNumber(row.locked_qty);
    available += availableQty(row);
    amount += toNumber(row.amount);
  }
  return { qty, locked, available, amount };
});

const stockPagination = computed(() => ({
  current: stockCurrent.value,
  pageSize: INVENTORY_PAGE_SIZE,
  total: stockTotal.value,
  showJumper: true,
  showPageSize: false,
}));

async function loadStock(page: number = stockCurrent.value): Promise<void> {
  stockLoading.value = true;
  stockError.value = '';
  try {
    const data = await fetchInventoryPage(buildStockQuery(stockFilter, page));
    stockRows.value = data.list ?? [];
    stockTotal.value = data.total ?? 0;
    stockCurrent.value = clampPage(data.page ?? page, stockTotal.value);
  } catch (error) {
    stockRows.value = [];
    stockTotal.value = 0;
    stockError.value = messageOf(error);
  } finally {
    stockLoading.value = false;
  }
}

function handleStockSearch(): void {
  void loadStock(1);
}

function handleStockReset(): void {
  Object.assign(stockFilter, emptyStockFilter());
  void loadStock(1);
}

function refreshStock(): void {
  void loadStock();
}

function handleStockPageChange(pageInfo: { current?: number }): void {
  void loadStock(pageInfo.current ?? 1);
}

// ==================== Tab2：库位库存 ====================

const locationColumns: PrimaryTableCol[] = [
  { colKey: 'location', title: '库位', minWidth: 160 },
  { colKey: 'material', title: '物料', minWidth: 200 },
  { colKey: 'warehouse', title: '仓库', width: 160 },
  { colKey: 'qty', title: '数量', width: 110, align: 'right' },
  { colKey: 'locked_qty', title: '锁定', width: 100, align: 'right' },
  { colKey: 'last_txn_at', title: '最近流水', width: 150 },
];

const locationRows = ref<InventoryLocationRow[]>([]);
const locationTotal = ref(0);
const locationCurrent = ref(1);
const locationLoading = ref(false);
const locationError = ref('');

const locationPagination = computed(() => ({
  current: locationCurrent.value,
  pageSize: INVENTORY_PAGE_SIZE,
  total: locationTotal.value,
  showJumper: true,
  showPageSize: false,
}));

async function loadLocation(page: number = locationCurrent.value): Promise<void> {
  locationLoading.value = true;
  locationError.value = '';
  try {
    const data = await fetchInventoryLocationPage(buildLocationQuery(locationFilter, page));
    locationRows.value = data.list ?? [];
    locationTotal.value = data.total ?? 0;
    locationCurrent.value = clampPage(data.page ?? page, locationTotal.value);
  } catch (error) {
    locationRows.value = [];
    locationTotal.value = 0;
    locationError.value = messageOf(error);
  } finally {
    locationLoading.value = false;
  }
}

function handleLocationSearch(): void {
  void loadLocation(1);
}

function handleLocationReset(): void {
  Object.assign(locationFilter, emptyLocationFilter());
  void loadLocation(1);
}

function refreshLocation(): void {
  void loadLocation();
}

function handleLocationPageChange(pageInfo: { current?: number }): void {
  void loadLocation(pageInfo.current ?? 1);
}

// ==================== Tab3：库存流水 ====================

const txnColumns: PrimaryTableCol[] = [
  { colKey: 'created_at', title: '时间', width: 150 },
  { colKey: 'txn_type', title: '类型', width: 90 },
  { colKey: 'biz_type', title: '业务类型', width: 130 },
  { colKey: 'qty', title: '数量', width: 110, align: 'right' },
  { colKey: 'unit_cost', title: '单位成本', width: 110, align: 'right' },
  { colKey: 'amount', title: '金额', width: 130, align: 'right' },
  { colKey: 'doc_no', title: '单据号', minWidth: 200 },
  { colKey: 'op', title: '操作', width: 80 },
];

const txnFilter = reactive<TxnFilterState>(emptyTxnFilter());
const txnRows = ref<InventoryTxnRow[]>([]);
const txnTotal = ref(0);
const txnCurrent = ref(1);
const txnLoading = ref(false);
const txnError = ref('');
/** 日期区间非法时的提示（不发请求） */
const txnHint = ref('');

/**
 * 方向下拉选项。
 *
 * `TXN_TYPE_OPTIONS` 在逻辑层是 `ReadonlyArray`（常量就该只读），而 `t-select` 的
 * `options` 声明是可变 `SelectOption[]` —— 直接传会 TS4104。这里摊平成可变数组，
 * 同时保证页面拿到的仍是同一份文案/值（单测钉的是逻辑层那一份）。
 */
const txnTypeOptions = computed(() => TXN_TYPE_OPTIONS.map((item) => ({ ...item })));

const txnPagination = computed(() => ({
  current: txnCurrent.value,
  pageSize: INVENTORY_PAGE_SIZE,
  total: txnTotal.value,
  showJumper: true,
  showPageSize: false,
}));

async function loadTxn(page: number = txnCurrent.value): Promise<void> {
  const rangeError = txnRangeError(txnFilter);
  if (rangeError) {
    txnHint.value = rangeError;
    txnRows.value = [];
    txnTotal.value = 0;
    return;
  }
  txnHint.value = '';
  txnLoading.value = true;
  txnError.value = '';
  try {
    const data = await fetchInventoryTxnPage(buildTxnQuery(txnFilter, page));
    txnRows.value = data.list ?? [];
    txnTotal.value = data.total ?? 0;
    txnCurrent.value = clampPage(data.page ?? page, txnTotal.value);
  } catch (error) {
    txnRows.value = [];
    txnTotal.value = 0;
    txnError.value = messageOf(error);
  } finally {
    txnLoading.value = false;
  }
}

function handleTxnSearch(): void {
  void loadTxn(1);
}

function handleTxnReset(): void {
  Object.assign(txnFilter, emptyTxnFilter());
  txnHint.value = '';
  void loadTxn(1);
}

function refreshTxn(): void {
  void loadTxn();
}

function handleTxnPageChange(pageInfo: { current?: number }): void {
  void loadTxn(pageInfo.current ?? 1);
}

// ==================== 流水详情抽屉 ====================

const txnDetailVisible = ref(false);
const txnDetail = ref<InventoryTxnRow | null>(null);
const txnDetailLoading = ref(false);

const snapshotColumns: PrimaryTableCol[] = [
  { colKey: 'name', title: '项目', width: 110 },
  { colKey: 'before', title: '变动前', align: 'right' },
  { colKey: 'after', title: '变动后', align: 'right' },
  { colKey: 'delta', title: '变动', align: 'right' },
];

/** 快照三行：数量 / 金额 / 均价（后端 before_* / after_* 六列） */
const snapshotRows = computed(() => {
  const row = txnDetail.value;
  if (!row) {
    return [];
  }
  const sign = row.txn_type === 'OUT' ? -1 : 1;
  const qtyDelta = sign * Math.abs(toNumber(row.qty));
  const amountDelta = sign * toNumber(row.amount);
  return [
    {
      k: 'qty',
      name: '数量',
      before: formatQty(row.before_qty),
      after: formatQty(row.after_qty),
      delta: `${qtyDelta > 0 ? '+' : ''}${formatQty(qtyDelta)}`,
      deltaClass: qtyDelta < 0 ? 'num--out' : 'num--in',
    },
    {
      k: 'amount',
      name: '金额',
      before: formatMoney(row.before_amount),
      after: formatMoney(row.after_amount),
      delta: `${amountDelta > 0 ? '+' : ''}${formatMoney(amountDelta)}`,
      deltaClass: amountDelta < 0 ? 'num--out' : 'num--in',
    },
    {
      k: 'cost',
      name: '均价',
      before: formatMoney(row.before_cost),
      after: formatMoney(row.after_cost),
      delta: '—',
      deltaClass: 'num--muted',
    },
  ];
});

/** 快照自洽（数量口径：`before ± qty = after`） */
const snapshotOk = computed(() => (txnDetail.value ? snapshotConsistent(txnDetail.value) : true));

/** 点行 / 点「详情」都开抽屉：列表行只有卡片字段，六列快照要靠详情接口 */
async function openTxnDetail(id: string): Promise<void> {
  txnDetailVisible.value = true;
  txnDetailLoading.value = true;
  try {
    txnDetail.value = await fetchInventoryTxnDetail(id);
  } catch (error) {
    txnDetail.value = null;
    txnError.value = messageOf(error);
  } finally {
    txnDetailLoading.value = false;
  }
}

function handleTxnRowClick(context: { row?: unknown }): void {
  const target = context?.row as InventoryTxnRow | undefined;
  if (target?.id) {
    void openTxnDetail(target.id);
  }
}

// ==================== Tab 切换 / 路由同步 ====================

/**
 * 切 Tab 时：先加载该 Tab 的数据，再把 URL 换成对应菜单路径（`router.push`）。
 *
 * 为什么要换 URL：侧边栏菜单按 `sys_menu.path` 高亮；在 `/business/erp/inventory` 下切到
 * 「库存流水」而地址还是 `/inventory`，用户刷新后又回 Tab1 —— 三处状态打脸。
 *
 * 为什么加载放在这里而不是等新实例：三条 route 共用**同一个组件**，Vue 复用组件实例
 * （不重新挂载，`onMounted` 不会再跑），所以切 Tab 的那一发请求必须由本实例发；
 * `loadedTabs` 兜住重复加载，切回已看过的 Tab 不会再打接口（要强制刷新用「刷新」按钮）。
 */
watch(activeTab, (tab) => {
  ensureLoaded(tab);
  const target = INVENTORY_TABS.find((item) => item.key === tab);
  if (target && route.path !== target.path) {
    void router.push({ path: target.path });
  }
});

/**
 * 点侧边栏换菜单路径进来（同一个组件实例，不会重新 mounted）→ 同步当前 Tab。
 *
 * 只改 `activeTab` 就够：`watch(activeTab)` 里 `route.path` 已经是目标路径（不会再 push），
 * 只会走 `ensureLoaded`，不会打环。
 */
watch(
  () => route.path,
  (path) => {
    const tab = tabFromMeta(route.meta?.tab, path);
    if (tab !== activeTab.value) {
      activeTab.value = tab;
    }
  },
);

/** 首次进入某个 Tab 才拉数据（切回来不重复请求，用「刷新」按钮强制） */
const loadedTabs = new Set<InventoryTabKey>();

function ensureLoaded(tab: InventoryTabKey): void {
  if (loadedTabs.has(tab)) {
    return;
  }
  loadedTabs.add(tab);
  if (tab === 'stock') {
    void loadStock(1);
  } else if (tab === 'location') {
    void loadLocation(1);
  } else {
    void loadTxn(1);
  }
}

onMounted(async () => {
  // 主数据拉不到不挡列表（只是名称列退化成长 id）
  const [materialPage, warehousePage, locationPage] = await Promise.all([
    fetchMaterialPage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
    fetchWarehousePage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
    fetchLocationPage({ page: 1, page_size: OPTION_LIMIT }).catch(() => null),
  ]);
  materials.value = materialPage?.list ?? [];
  warehouses.value = warehousePage?.list ?? [];
  locations.value = locationPage?.list ?? [];
  ensureLoaded(activeTab.value);
});
</script>

<style scoped lang="less">
.inventory-page {
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

.tool-row__select {
  width: 220px;
}

.tool-row__type {
  width: 140px;
}

.tool-row__doc {
  width: 200px;
}

.tool-row__date {
  width: 150px;
}

.tool-row__sep {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.tool-row__switch {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  color: var(--td-text-color-secondary);
  cursor: pointer;
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

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.panel-head__tip {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.panel-head__tip code {
  padding: 1px 4px;
  border-radius: 3px;
  background: var(--td-bg-color-container-hover);
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.list-alert {
  margin-bottom: 10px;
}

/* ==================== 单元格 ==================== */

.cell-stack {
  display: inline-flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.cell-stack__main {
  color: var(--td-text-color-primary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.cell-stack__sub {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.num {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
  color: var(--td-text-color-primary);
  font-variant-numeric: tabular-nums;
}

.num--muted {
  color: var(--td-text-color-placeholder);
}

.num--zero {
  color: var(--td-text-color-placeholder);
}

.num--bad {
  color: var(--td-error-color);
  font-weight: 600;
}

.num--in {
  color: var(--td-success-color);
}

.num--out {
  color: var(--td-warning-color);
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

/* ==================== 页脚合计 ==================== */

.footer-row {
  display: flex;
  align-items: center;
  gap: 20px;
  padding: 10px 16px;
  border: 1px solid var(--td-component-stroke);
  border-top: none;
  border-radius: 0 0 6px 6px;
  background: var(--td-bg-color-container-hover);
  font-size: 14px;
}

.footer-row__label {
  color: var(--td-text-color-secondary);
}

.footer-row__item {
  color: var(--td-text-color-secondary);
}

.footer-row__item b {
  margin-left: 6px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.footer-row__tip {
  margin-left: auto;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* ==================== 抽屉 ==================== */

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
  min-width: 0;
}

.drawer-head__title {
  font-size: 15px;
  font-weight: 600;
  color: var(--td-text-color-primary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.drawer-head__side {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: 0 0 auto;
}

.drawer-body {
  padding-top: 12px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px 16px;
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

.drawer-section {
  margin: 8px 0 0;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.drawer-note {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 4px 0 0;
  font-size: 12px;
}

.drawer-note--ok {
  color: var(--td-success-color);
}

.drawer-note--bad {
  color: var(--td-error-color);
}
</style>
