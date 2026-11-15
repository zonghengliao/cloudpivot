<template>
  <div class="inventory-page">
    <div class="inventory-tabs">
      <t-tabs v-model="activeTab" data-testid="inventory-tabs">
        <!-- ==================== Tab 1：即时库存 ==================== -->
        <t-tab-panel value="stock" label="即时库存">
          <!--
            ★ 缺陷①（P2-2B）：后端巡检提醒的「去看看」落点是 ?audit=1（period.rs 的
            Blocker::with_url 给的就是这条），此前前端不读这个参数 —— 点进来和普通即时库存
            一模一样，仓管根本看不到漂移物料。这里消费它：只列「账面 ≠ 在库序列号数」的
            （物料 × 仓库），数据来自 GET /erp/inventory/audit?dry_run=true。
            页面**只读**：一键改账不该是个页面按钮（后端红线③：dry_run 缺省 true 防误修）。
          -->
          <t-card v-if="auditMode" :bordered="false" class="list-card">
            <t-alert theme="info" class="list-alert audit-banner" data-testid="inventory-audit-banner">
              <template #title>巡检模式</template>
              <span data-testid="inventory-audit-summary">{{ auditSummary }}</span>
              <template #operation>
                <t-space size="small">
                  <t-button
                    size="small"
                    variant="outline"
                    :loading="auditLoading"
                    data-testid="btn-inventory-audit-refresh"
                    @click="loadAudit()"
                  >
                    重新巡检
                  </t-button>
                  <t-button
                    size="small"
                    theme="primary"
                    variant="outline"
                    data-testid="btn-inventory-audit-exit"
                    @click="exitAuditMode"
                  >
                    退出巡检
                  </t-button>
                </t-space>
              </template>
            </t-alert>

            <t-alert
              v-if="auditError"
              theme="error"
              :message="auditError"
              class="list-alert"
              data-testid="inventory-audit-error"
            >
              <template #operation>
                <t-button size="small" variant="outline" @click="loadAudit()">重试</t-button>
              </template>
            </t-alert>

            <div class="panel-head">
              <div class="panel-head__main">
                <span class="panel-head__title">漂移明细（账面 vs 在库序列号）</span>
                <span class="panel-head__meta" data-testid="inventory-audit-meta">
                  漂移 {{ auditReport?.mismatched ?? 0 }} 个 / 检查 {{ auditReport?.checked ?? 0 }} 个
                </span>
              </div>
              <span class="panel-head__tip">
                口径：<code>SUM(biz_inventory.qty)</code>（该物料×仓库的「所有桶之和」）对比
                <code>biz_material_serial.status='IN_STOCK'</code> 的件数；修正走巡检接口
                （<code>dry_run=false</code>，单桶按差额调整、多桶交人工），本页只读
              </span>
            </div>

            <t-table
              row-key="key"
              :data="auditRows"
              :columns="auditColumns"
              :loading="auditLoading"
              size="small"
              hover
              data-testid="inventory-audit-table"
            >
              <template #material="{ row }">
                <div class="cell-stack">
                  <span class="cell-stack__main mono" :data-testid="`audit-material-${row.material_code}`">
                    {{ row.material_code }}
                  </span>
                  <span class="cell-stack__sub">{{ row.material_name }}</span>
                </div>
              </template>
              <template #warehouse="{ row }">
                <span v-if="row.warehouse_id" class="cell-stack__main">{{ row.warehouse_name }}</span>
                <t-tag v-else size="small" variant="light" theme="warning">无仓库归属</t-tag>
              </template>
              <template #book_qty="{ row }">
                <span class="num">{{ formatQty(row.book_qty) }}</span>
              </template>
              <template #bucket_count="{ row }">
                <span
                  class="num muted"
                  :data-testid="`audit-buckets-${row.material_code}`"
                >{{ row.bucket_count }}</span>
              </template>
              <template #actual_qty="{ row }">
                <span class="num">{{ formatQty(row.actual_qty) }}</span>
              </template>
              <template #diff="{ row }">
                <span
                  class="num"
                  :class="auditDriftKind(row.diff) === 'over' ? 'num--bad' : 'num--out'"
                  :data-testid="`audit-diff-${row.material_code}`"
                >
                  {{ auditDiffText(row.diff) }}
                </span>
              </template>
              <template #drift="{ row }">
                <t-tag size="small" variant="light" :theme="auditDriftTheme(row.diff)">
                  {{ auditDriftLabel(row.diff) }}
                </t-tag>
              </template>
              <template #fixable="{ row }">
                <span class="muted" :data-testid="`audit-fixable-${row.material_code}`">
                  {{ auditFixableLabel(row.fixable, row.unfixable_reason) }}
                </span>
              </template>

              <template #empty>
                <div class="panel-empty" data-testid="inventory-audit-empty">
                  <t-icon name="check-circle" class="panel-empty__icon" />
                  <p class="panel-empty__title">{{ auditEmptyTitle }}</p>
                  <p class="panel-empty__desc">{{ auditEmptyDesc }}</p>
                </div>
              </template>
            </t-table>
          </t-card>

          <!-- 列表：筛选 / 分页 / 导出交给 ListPage（P3-07） -->
          <ListPage
            v-else
            ref="stockListRef"
            title="即时库存"
            testid="stock"
            table-testid="stock-table"
            meta-testid="stock-meta"
            btn-prefix="stock-btn"
            :columns="stockColumns"
            :filters="stockFilters"
            :filter-state="stockFilter"
            :fetcher="stockFetcher"
            :query-builder="stockQueryBuilder"
            :page-size="INVENTORY_PAGE_SIZE"
            exportable
            @loaded="onStockLoaded"
          >
            <!-- 「隐藏零库存」不是下拉 / 输入框：走 `custom` 筛选槽，重置时由组件写成 false -->
            <template #filter-only_nonzero>
              <label class="tool-row__switch">
                <t-switch v-model="stockFilter.only_nonzero" size="small" data-testid="stock-switch-nonzero" />
                <span>隐藏零库存</span>
              </label>
            </template>

            <template #head-extra>
              <span class="panel-head__tip">
                数量 = 已过账的账面结存（可用 = 数量；「锁定量」为预留字段，本版本无占用流程，故不显示）
              </span>
            </template>

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

              <template #footer>
                <div class="footer-row" data-testid="stock-footer">
                  <span class="footer-row__label">本页合计</span>
                  <span class="footer-row__item">
                    数量 <b class="num" data-testid="stock-footer-qty">{{ formatQty(stockTotals.qty) }}</b>
                  </span>
                  <span class="footer-row__item">
                    可用 <b class="num" data-testid="stock-footer-available">{{ formatQty(stockTotals.available) }}</b>
                  </span>
                  <span class="footer-row__item">
                    金额 <b class="num" data-testid="stock-footer-amount">{{ formatMoney(stockTotals.amount) }}</b>
                  </span>
                  <span class="footer-row__tip">合计只算本页，不是全仓库存</span>
                </div>
              </template>
            </ListPage>
        </t-tab-panel>

        <!-- ==================== Tab 2：库位库存 ==================== -->
        <t-tab-panel value="location" label="库位库存" lazy>
          <ListPage
            ref="locationListRef"
            title="库位库存"
            testid="location"
            table-testid="location-table"
            meta-testid="location-meta"
            btn-prefix="location-btn"
            :columns="locationColumns"
            :filters="locationFilters"
            :filter-state="locationFilter"
            :fetcher="locationFetcher"
            :query-builder="locationQueryBuilder"
            :page-size="INVENTORY_PAGE_SIZE"
            exportable
            @loaded="onLocationLoaded"
          >
            <template #head-extra>
              <span class="panel-head__tip">
                主表 <code>biz_inventory_location</code>；库位列为空 = 该仓未分配到具体库位的数量
              </span>
            </template>

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
          </ListPage>
        </t-tab-panel>

        <!-- ==================== Tab 3：库存流水 ==================== -->
        <t-tab-panel value="txn" label="库存流水" lazy>
          <ListPage
            ref="txnListRef"
            title="库存流水"
            testid="txn"
            table-testid="txn-table"
            meta-testid="txn-meta"
            btn-prefix="txn-btn"
            :columns="txnColumns"
            :filters="txnFilters"
            :filter-state="txnFilter"
            :fetcher="txnFetcher"
            :query-builder="txnQueryBuilder"
            :page-size="INVENTORY_PAGE_SIZE"
            exportable
            @loaded="onTxnLoaded"
            @row-click="handleTxnRowClick"
          >
            <!-- 日期区间整块画在一个槽里：两个日期框分属两个槽时，换行会把「起 / 止」拆到两行 -->
            <template #filter-date_from>
              <span class="lp-date-range">
                <t-date-picker
                  :model-value="txnFilter.date_from"
                  class="tool-row__date"
                  placeholder="起始日"
                  format="YYYY-MM-DD"
                  value-type="YYYY-MM-DD"
                  allow-input
                  clearable
                  data-testid="txn-filter-from"
                  @update:model-value="setTxnDateFrom"
                />
                <span class="tool-row__sep">到</span>
                <t-date-picker
                  :model-value="txnFilter.date_to"
                  class="tool-row__date"
                  placeholder="结束日"
                  format="YYYY-MM-DD"
                  value-type="YYYY-MM-DD"
                  allow-input
                  clearable
                  data-testid="txn-filter-to"
                  @update:model-value="setTxnDateTo"
                />
              </span>
            </template>

            <template #filter-footer>
              <t-alert v-if="txnHint" theme="warning" :message="txnHint" class="list-alert" data-testid="txn-hint" />
            </template>

            <template #head-extra>
              <span class="panel-head__tip">
                主表 <code>biz_inventory_txn</code>；数量恒为正，方向看「类型」；点行看 before/after 快照
              </span>
            </template>

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
          </ListPage>
        </t-tab-panel>
      </t-tabs>
    </div>

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
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import {
  fetchInventoryAudit,
  fetchInventoryLocationPage,
  fetchInventoryPage,
  fetchInventoryTxnDetail,
  fetchInventoryTxnPage,
  fetchLocationPage,
  fetchMaterialPage,
  fetchWarehousePage,
  type InventoryAuditReport,
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
  auditDiffText,
  auditDriftKind,
  auditDriftLabel,
  auditDriftTheme,
  auditEmptyText,
  auditFixableLabel,
  auditSummaryText,
  availableQty,
  buildLocationQuery,
  buildStockQuery,
  buildTxnQuery,
  bizTypeLabel,
  docTypeLabel,
  emptyLocationFilter,
  emptyStockFilter,
  emptyTxnFilter,
  isAuditQuery,
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
  // 「锁定」列已按 P2-2 ③ 撤掉：`locked_qty` 是预留列（无占用流程，恒为 0），
  // 显示一列永远是 0 的数字会让人以为占用机制在工作。available 保留下是因为
  // 占用流程落地后它才真正不等于数量（见 docs/ERP-摆设字段口径.md）。
  { colKey: 'available', title: '可用', width: 110, align: 'right' },
  { colKey: 'avg_cost', title: '均价', width: 110, align: 'right' },
  { colKey: 'amount', title: '金额', width: 130, align: 'right' },
  { colKey: 'last_txn_at', title: '最近流水', width: 150 },
];

const stockFilter = reactive<StockFilterState>(emptyStockFilter());
/** 本页行（镜像 ListPage 抛回来的这一页：页脚合计要算它） */
const stockRows = ref<InventoryRow[]>([]);
/** 当前页 */
const stockCurrent = ref(1);
/** ListPage 实例（巡检模式进出时用） */
const stockListRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 本页合计（数量 / 可用 / 金额），只算本页 */
const stockTotals = computed(() => {
  let qty = 0;
  let available = 0;
  let amount = 0;
  for (const row of stockRows.value) {
    qty += toNumber(row.qty);
    available += availableQty(row);
    amount += toNumber(row.amount);
  }
  return { qty, available, amount };
});

/**
 * Tab1 筛选项声明。
 *
 * 两个下拉改造前**不**触发即时查询（只靠「查询」按钮），所以不开 `searchOnChange`；
 * 用 `computed` 是因为选项来自异步主数据。
 */
const stockFilters = computed<FilterDef[]>(() => [
  {
    key: 'material_id',
    type: 'select',
    placeholder: '物料（全部）',
    width: 220,
    options: materialOptions.value,
    filterable: true,
    testid: 'stock-filter-material',
  },
  {
    key: 'warehouse_id',
    type: 'select',
    placeholder: '仓库（全部）',
    width: 220,
    options: warehouseOptions.value,
    filterable: true,
    testid: 'stock-filter-warehouse',
  },
  // 「隐藏零库存」开关：走 `#filter-only_nonzero` 插槽自绘，重置时由组件写回 `false`
  { key: 'only_nonzero', type: 'custom', defaultValue: false, testid: 'stock-switch-nonzero' },
]);

const stockFetcher: ListFetcher<InventoryRow> = (query) =>
  fetchInventoryPage(query as Parameters<typeof fetchInventoryPage>[0]);

const stockQueryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildStockQuery(state as StockFilterState, page, pageSize);

/** Tab1 一页拉完：镜像行 + 页码 */
function onStockLoaded(payload: { rows: InventoryRow[]; total: number; page: number }): void {
  stockRows.value = payload.rows;
  stockCurrent.value = payload.page;
}

// ==================== Tab2：库位库存 ====================

const locationColumns: PrimaryTableCol[] = [
  { colKey: 'location', title: '库位', minWidth: 160 },
  { colKey: 'material', title: '物料', minWidth: 200 },
  { colKey: 'warehouse', title: '仓库', width: 160 },
  { colKey: 'qty', title: '数量', width: 110, align: 'right' },
  { colKey: 'last_txn_at', title: '最近流水', width: 150 },
];

const locationRows = ref<InventoryLocationRow[]>([]);
/** 当前页 */
const locationCurrent = ref(1);
const locationListRef = ref<InstanceType<typeof ListPage> | null>(null);

/** Tab2 筛选项（库位下拉按 Tab2 所选仓库收窄，所以是 computed） */
const locationFilters = computed<FilterDef[]>(() => [
  {
    key: 'warehouse_id',
    type: 'select',
    placeholder: '仓库（全部）',
    width: 220,
    options: warehouseOptions.value,
    filterable: true,
    testid: 'location-filter-warehouse',
  },
  {
    key: 'material_id',
    type: 'select',
    placeholder: '物料（全部）',
    width: 220,
    options: materialOptions.value,
    filterable: true,
    testid: 'location-filter-material',
  },
  {
    key: 'location_id',
    type: 'select',
    placeholder: '库位（全部）',
    width: 220,
    options: locationOptions.value,
    filterable: true,
    testid: 'location-filter-location',
  },
]);

const locationFetcher: ListFetcher<InventoryLocationRow> = (query) =>
  fetchInventoryLocationPage(query as Parameters<typeof fetchInventoryLocationPage>[0]);

const locationQueryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildLocationQuery(state as LocationFilterState, page, pageSize);

/** Tab2 一页拉完：镜像行 + 页码（本 Tab 没有页脚合计，镜像只为「刷新当前页」留口） */
function onLocationLoaded(payload: { rows: InventoryLocationRow[]; page: number }): void {
  locationRows.value = payload.rows;
  locationCurrent.value = payload.page;
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
/** 当前页 */
const txnCurrent = ref(1);
const txnListRef = ref<InstanceType<typeof ListPage> | null>(null);
/** 日期区间非法时的提示（不发请求，条子在筛选卡下方） */
const txnHint = ref('');

/**
 * 方向下拉选项。
 *
 * `TXN_TYPE_OPTIONS` 在逻辑层是 `ReadonlyArray`（常量就该只读），而 `t-select` 的
 * `options` 声明是可变 `SelectOption[]` —— 直接传会 TS4104。这里摊平成可变数组，
 * 同时保证页面拿到的仍是同一份文案/值（单测钉的是逻辑层那一份）。
 */
const txnTypeOptions = computed(() => TXN_TYPE_OPTIONS.map((item) => ({ ...item })));

/** Tab3 筛选项（日期区间整块画在 `#filter-date_from` 槽里） */
const txnFilters = computed<FilterDef[]>(() => [
  {
    key: 'material_id',
    type: 'select',
    placeholder: '物料（全部）',
    width: 220,
    options: materialOptions.value,
    filterable: true,
    testid: 'txn-filter-material',
  },
  {
    key: 'warehouse_id',
    type: 'select',
    placeholder: '仓库（全部）',
    width: 220,
    options: warehouseOptions.value,
    filterable: true,
    testid: 'txn-filter-warehouse',
  },
  {
    key: 'txn_type',
    type: 'select',
    placeholder: '类型（全部）',
    width: 140,
    options: txnTypeOptions.value,
    testid: 'txn-filter-type',
  },
  {
    key: 'doc_no',
    type: 'keyword',
    placeholder: '单据号（精确）',
    width: 200,
    testid: 'txn-filter-docno',
  },
  // 起 / 止两个受控日期框：重置时组件按 `defaultValue` 写回空串（不能是 undefined，否则日期框不受控）
  { key: 'date_from', type: 'custom', defaultValue: '', testid: 'txn-filter-from' },
  { key: 'date_to', type: 'custom', defaultValue: '', testid: 'txn-filter-to' },
]);

/**
 * 拉一页。
 *
 * 日期区间非法（起 > 止）时**不发请求**：给黄条 + 空列表，与改造前 `loadTxn` 的早退一致。
 */
const txnFetcher: ListFetcher<InventoryTxnRow> = async (query) => {
  const rangeError = txnRangeError(txnFilter);
  txnHint.value = rangeError;
  if (rangeError) {
    return { list: [], total: 0, page: query.page ?? 1, page_size: query.page_size ?? INVENTORY_PAGE_SIZE };
  }
  return fetchInventoryTxnPage(query as Parameters<typeof fetchInventoryTxnPage>[0]);
};

const txnQueryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildTxnQuery(state as TxnFilterState, page, pageSize);

/** Tab3 一页拉完：镜像行 + 页码 */
function onTxnLoaded(payload: { rows: InventoryTxnRow[]; page: number }): void {
  txnRows.value = payload.rows;
  txnCurrent.value = payload.page;
}

/** 日期框写回（`t-date-picker` 的 `update:model-value` 可能是 `null`，一律归成空串） */
function setTxnDateFrom(value: unknown): void {
  txnFilter.date_from = value == null ? '' : String(value);
}

function setTxnDateTo(value: unknown): void {
  txnFilter.date_to = value == null ? '' : String(value);
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
    // 详情拉不到只弹一条 toast：列表本身是好的，别把整块列表染红
    MessagePlugin.error(`流水详情加载失败：${messageOf(error)}`);
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

// ==================== ④ 巡检模式（`?audit=1`） ====================

/**
 * 巡检模式判定。
 *
 * 落点来自后端：`period.rs::global_health_warnings`（结账预检）/ `period_items.rs` /
 * `year_end.rs` 的 `Blocker::with_url` 都给 `/business/erp/inventory?audit=1`。
 * 「点『去看看』→ 页面只列漂移物料」这条链路此前断在前端（参数没人读），本块接上。
 */
const auditMode = computed(() => isAuditQuery(route.query.audit));

/** 巡检报告（`null` = 还没取到） */
const auditReport = ref<InventoryAuditReport | null>(null);
const auditLoading = ref(false);
const auditError = ref('');

const auditColumns: PrimaryTableCol[] = [
  { colKey: 'material', title: '物料', minWidth: 220 },
  { colKey: 'warehouse', title: '仓库', width: 180 },
  { colKey: 'bucket_count', title: '成本账桶数', width: 110, align: 'right' },
  { colKey: 'book_qty', title: '账面数量', width: 120, align: 'right' },
  { colKey: 'actual_qty', title: '在库件数', width: 120, align: 'right' },
  { colKey: 'diff', title: '差异', width: 100, align: 'right' },
  { colKey: 'drift', title: '状态', width: 100 },
  { colKey: 'fixable', title: '可否修正', width: 150 },
];

/**
 * 表格行：`key` 用「物料 + 仓库 + 序号」拼。
 * 同一物料在多仓各有一行（`material_id` 会撞），不能拿它当 row-key。
 */
const auditRows = computed(() =>
  (auditReport.value?.items ?? []).map((item, index) => ({
    ...item,
    key: `${item.material_id}-${item.warehouse_id ?? 'no-warehouse'}-${index}`,
  })),
);

/** 顶部一句话结论（未取到前给加载态文案，别显示「发现 0 个漂移」这种假结论） */
const auditSummary = computed(() => {
  if (auditReport.value) {
    return auditSummaryText(auditReport.value);
  }
  return auditLoading.value ? '正在巡检（账面 vs 在库序列号）…' : '尚未取到巡检结果';
});

const auditEmptyTitle = computed(() =>
  auditReport.value && auditReport.value.mismatched === 0 ? '没有漂移' : '没有可显示的明细',
);
const auditEmptyDesc = computed(() => auditEmptyText(auditReport.value ?? { checked: 0, mismatched: 0 }));

/**
 * 拉巡检报告（只读口径 `dry_run=true`）。
 *
 * 权限码 `business:erp:inventory:audit`（V0143）。没这个码的账号会 403 —— 把后端原文
 * 照出来（而不是一句「加载失败」）：能不能看巡检是个用户需要知道的事实。
 */
async function loadAudit(): Promise<void> {
  auditLoading.value = true;
  auditError.value = '';
  try {
    auditReport.value = await fetchInventoryAudit();
  } catch (error) {
    auditReport.value = null;
    auditError.value = messageOf(error);
  } finally {
    auditLoading.value = false;
  }
}

/** 退出巡检：只摘掉 `audit` 参数（其它 query 原样保留），回到普通即时库存 */
function exitAuditMode(): void {
  const rest = { ...route.query };
  delete rest.audit;
  void router.replace({ path: route.path, query: rest });
}

/**
 * 进出巡检模式：进来就拉报告，退出就丢掉报告。
 *
 * 退出后 Tab1 换回 ListPage（`v-else` 重新挂载），首屏那一发由组件自己发 —— 这里不再手动
 * `loadStock`，否则退出瞬间会打两次同样的接口。
 */
watch(auditMode, (on) => {
  if (on) {
    void loadAudit();
    return;
  }
  auditReport.value = null;
  auditError.value = '';
});

// ==================== Tab 切换 / 路由同步 ====================

/**
 * 切 Tab 时把 URL 换成对应菜单路径（`router.push`）。
 *
 * 为什么要换 URL：侧边栏菜单按 `sys_menu.path` 高亮；在 `/business/erp/inventory` 下切到
 * 「库存流水」而地址还是 `/inventory`，用户刷新后又回 Tab1 —— 三处状态打脸。
 *
 * 数据加载不用在这里管：三个 Tab 的 `t-tab-panel` 都标了 `lazy`，面板第一次显示时
 * 才挂载里面的 ListPage，由它自己拉首屏（切回已看过的 Tab 只是重新显示，不会再打接口）。
 */
watch(activeTab, (tab) => {
  const target = INVENTORY_TABS.find((item) => item.key === tab);
  if (target && route.path !== target.path) {
    // query 一并带过去：巡检模式（`?audit=1`）下切 Tab 不能被顺手清掉，
    // 否则「切到库位看一眼再切回来」就悄悄退出了巡检。
    void router.push({ path: target.path, query: route.query });
  }
});

/**
 * 点侧边栏换菜单路径进来（同一个组件实例，不会重新 mounted）→ 同步当前 Tab。
 *
 * 只改 `activeTab` 就够：`watch(activeTab)` 里 `route.path` 已经是目标路径（不会再 push），
 * 不会打环。
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
  // 巡检模式（`?audit=1`）直达：Tab1 渲染的是巡检报告，不会去挂 ListPage
  if (auditMode.value) {
    void loadAudit();
  }
});
</script>

<style scoped lang="less">
.inventory-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 三个 Tab 各自带一套筛选卡 + 列表卡（ListPage），所以外层只是布局容器，不再是 card */
.inventory-tabs {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

/* 日期区间整块（起始日 ~ 结束日）：作为一个 flex item，换行时整体走，不会被拆开 */
.lp-date-range {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  white-space: nowrap;
}

.tool-row__switch {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  color: var(--td-text-color-secondary);
  cursor: pointer;
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
