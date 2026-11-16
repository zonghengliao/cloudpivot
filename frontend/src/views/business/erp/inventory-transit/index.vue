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
        <!-- ==================== Tab 3：在途对账（M2-07b；按 audit 码显隐） ==================== -->
        <!--
          `v-if="canAudit"` 而不是整页 403：对账是独立授权面（V0422 的 :audit 按钮行），
          没有这个码的角色只是看不到这个 Tab，明细 / 汇总照常。
          `lazy`：第一次切过去才跑对账（全量扫描 2000 组，别在首屏白跑）。
        -->
        <t-tab-panel v-if="canAudit" :value="RECONCILE_TAB.key" :label="RECONCILE_TAB.label" lazy>
          <t-card :bordered="false" class="transit-reconcile" data-testid="transit-reconcile">
            <div class="tool-row">
              <t-select
                v-model="reconcileFilter.shipment_id"
                class="transit-tool__select"
                :options="shipmentOptions"
                placeholder="发货单号（全部）"
                filterable
                clearable
                data-testid="transit-reconcile-shipment"
              />
              <t-select
                v-model="reconcileFilter.material_id"
                class="transit-tool__select"
                :options="materialOptions"
                placeholder="物料（全部）"
                filterable
                clearable
                data-testid="transit-reconcile-material"
              />
              <t-select
                v-model="reconcileFilter.warehouse_id"
                class="transit-tool__select"
                :options="warehouseOptions"
                placeholder="发货仓库（全部）"
                filterable
                clearable
                data-testid="transit-reconcile-warehouse"
              />
              <t-date-range-picker
                :model-value="reconcileFilter.date_range"
                class="transit-tool__range"
                placeholder="发货日期区间"
                format="YYYY-MM-DD"
                clearable
                data-testid="transit-reconcile-date"
                @update:model-value="setReconcileDateRange"
              />
              <label class="transit-check" data-testid="transit-reconcile-include-matched-label">
                <t-switch v-model="reconcileFilter.include_matched" size="small" data-testid="transit-reconcile-include-matched" />
                <span class="transit-check__text">连对平的组一起看</span>
              </label>
              <t-button theme="primary" :loading="reconcileLoading" data-testid="transit-reconcile-run" @click="runReconcile()">
                开始对账
              </t-button>
              <t-button variant="outline" data-testid="transit-reconcile-reset" @click="resetReconcile()">重置</t-button>
            </div>

            <span class="transit-tip">
              口径：发货单明细 Σqty（SSOT）= 已出 + 在途，逐「发货单 × 物料」比对；缺省只列「有差异」的组。
              三侧数量与差额全部取后端（页面不做加减）；扫描上限 <code>{{ reconcile?.scan_limit ?? '—' }}</code> 组。
            </span>

            <t-alert
              v-if="reconcileError"
              theme="error"
              :message="reconcileError"
              class="transit-alert"
              data-testid="transit-reconcile-error"
            >
              <template #operation>
                <t-button size="small" variant="outline" @click="runReconcile()">重试</t-button>
              </template>
            </t-alert>

            <!-- 结论横幅：一句话说清平不平（三侧合计与差额取后端 totals） -->
            <t-alert
              v-if="reconcileBanner"
              :theme="reconcileBanner.theme"
              :title="reconcileBanner.title"
              :message="reconcileBanner.desc"
              class="transit-alert"
              data-testid="transit-reconcile-banner"
            />

            <t-alert
              v-if="reconcileTruncatedHint"
              theme="warning"
              :message="reconcileTruncatedHint"
              class="transit-alert"
              data-testid="transit-reconcile-truncated"
            />

            <!-- KPI：全部取后端 totals（页面零加法） -->
            <div v-if="reconcile" class="transit-kpi" data-testid="transit-reconcile-kpi">
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">比对组数</span>
                <b class="transit-kpi__value" data-testid="transit-reconcile-checked">{{ reconcile.totals.checked }}</b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">对平组数</span>
                <b class="transit-kpi__value" data-testid="transit-reconcile-matched">{{ reconcile.totals.matched }}</b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">不平组数</span>
                <b class="transit-kpi__value" data-testid="transit-reconcile-mismatched">{{ reconcile.totals.mismatched }}</b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">整批作废组数</span>
                <b class="transit-kpi__value" data-testid="transit-reconcile-cancelled">{{ reconcile.totals.cancelled }}</b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">未纳入在途账</span>
                <b class="transit-kpi__value" data-testid="transit-reconcile-untracked">{{ reconcile.totals.untracked }}</b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">发货总量（已纳入）</span>
                <b class="transit-kpi__value" data-testid="transit-reconcile-total-item">{{ formatQty(reconcile.totals.item_qty) }}</b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">已出合计</span>
                <b class="transit-kpi__value" data-testid="transit-reconcile-total-received">{{ formatQty(reconcile.totals.received_qty) }}</b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">在途合计</span>
                <b class="transit-kpi__value" data-testid="transit-reconcile-total-transit">{{ formatQty(reconcile.totals.in_transit_qty) }}</b>
              </div>
              <div class="transit-kpi__item">
                <span class="transit-kpi__label">差异量</span>
                <b
                  class="transit-kpi__value"
                  :class="{ 'transit-kpi__value--zero': isZeroQty(reconcile.totals.diff) }"
                  data-testid="transit-reconcile-total-diff"
                >
                  {{ formatQty(reconcile.totals.diff) }}
                </b>
              </div>
            </div>

            <t-loading :loading="reconcileLoading" size="small">
              <t-table
                v-if="reconcileRows.length"
                row-key="__key"
                :data="reconcileRows"
                :columns="reconcileColumns"
                size="small"
                hover
                max-height="560"
                data-testid="transit-reconcile-table"
              >
                <template #ship_no="{ row }">
                  <div class="cell-stack">
                    <span class="cell-stack__main">
                      <!-- 可点进发货单：没有发货单读码（:shipment:list）时退化成纯文本，不做「点了 403」的链接 -->
                      <t-link
                        v-if="canOpenShipment"
                        theme="primary"
                        class="mono"
                        :data-testid="`transit-reconcile-shipment-link-${row.shipment_id}`"
                        @click="openShipment(row)"
                      >
                        {{ dash(row.ship_no) }}
                      </t-link>
                      <span v-else class="mono" :data-testid="`transit-reconcile-shipment-text-${row.shipment_id}`">
                        {{ dash(row.ship_no) }}
                      </span>
                      <t-tag v-if="row.shipment_deleted" size="small" variant="light" theme="default" class="transit-reconcile__deleted">
                        发货单已删
                      </t-tag>
                    </span>
                    <span class="cell-stack__sub mono">{{ dash(row.ship_date) }}</span>
                  </div>
                </template>

                <template #material="{ row }">
                  <div class="cell-stack">
                    <span class="cell-stack__main" :data-testid="`transit-reconcile-material-${row.material_id}`">
                      {{ materialText(row) }}
                    </span>
                    <span class="cell-stack__sub">{{ dash(row.spec) }}</span>
                  </div>
                </template>

                <template #warehouse="{ row }">
                  <span>{{ reconcileWarehouseText(row) }}</span>
                </template>

                <template #item_qty="{ row }">
                  <span class="num">{{ formatQty(row.item_qty) }}</span>
                </template>

                <template #received_qty="{ row }">
                  <span class="num num--muted">{{ formatQty(row.received_qty) }}</span>
                </template>

                <template #qty_in_transit="{ row }">
                  <span class="num num--muted">{{ formatQty(row.qty_in_transit) }}</span>
                </template>

                <template #diff="{ row }">
                  <span
                    class="num num--strong"
                    :class="{ 'num--zero': isZeroQty(row.diff), 'num--danger': !isZeroQty(row.diff) }"
                    :data-testid="`transit-reconcile-diff-${row.shipment_id}-${row.material_id}`"
                  >
                    {{ diffText(row) }}
                  </span>
                </template>

                <template #issues="{ row }">
                  <div class="transit-reconcile__issues">
                    <t-tag
                      size="small"
                      variant="light"
                      :theme="reconcileResultTheme(row)"
                      :data-testid="`transit-reconcile-result-${row.shipment_id}-${row.material_id}`"
                    >
                      {{ reconcileResultText(row) }}
                    </t-tag>
                    <!-- 后端差异文案是一句话（不是词条）：用普通文本，窄屏也能换行；t-tag 的 nowrap 会把它截断。
                         对平行不再重复一遍「对平」（判定 tag 已经说了） -->
                    <span
                      v-if="!row.matched"
                      class="transit-reconcile__issue-text"
                      :data-severity="issueTheme(row.issues[0])"
                    >
                      {{ issueText(row) }}
                    </span>
                  </div>
                </template>
              </t-table>

              <div v-else-if="!reconcileLoading && !reconcileError" class="transit-empty" data-testid="transit-reconcile-empty">
                <t-icon name="swap" class="transit-empty__icon" />
                <p class="transit-empty__title" data-testid="transit-reconcile-empty-title">{{ reconcileEmpty.title }}</p>
                <p class="transit-empty__desc">{{ reconcileEmpty.desc }}</p>
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
 * 在途库存页：**在途明细 + 在途汇总** 两个常驻 Tab，外加**在途对账**（M2-07b）。
 *
 * # 数据源只有三个只读接口（都在后端 `inventory_transit` 模块）
 *
 * ```text
 * Tab1 在途明细   GET /business/erp/inventory-transit/page       TransitRow[]（分页 + 筛选）
 * Tab2 在途汇总   GET /business/erp/inventory-transit/summary    按 物料 × 发货仓（+ 全量合计）
 * Tab3 在途对账   GET /business/erp/inventory-transit/reconcile  逐 发货单 × 物料 的三侧数量 + 差异码
 * ```
 *
 * 页面**不算账**：
 * * `qty_in_transit` 是 PG 生成列（`shipped_qty - received_qty`），任何写入路径都写不出「不符」的值；
 * * `age_days` / `max_age_days` 由 PG 按 `CURRENT_DATE - ship_date` 算（浏览器时区不参与）；
 * * 汇总的 `total_*` 是后端**另一条聚合查询**算的，`truncated = true` 时行不全但合计仍是全量；
 * * 对账的三侧数量（`item_qty` / `received_qty` / `qty_in_transit`）、差额 `diff` 与 `totals.*`
 *   一律取后端（`reconcile::ComboRow::diff` 是唯一口径）—— 页面**没有任何 `reduce` / 减法**：
 *   自己算一遍 = 第二套账，而且对账要的正是「前端独立算出来会不会跟后端不一样」这个否证。
 *
 * # 三个权限码（别指望一个）
 *
 * | 码 | 挂在哪 | 管什么 |
 * |----|--------|--------|
 * | `business:erp:inventory-transit:page` | V0423 菜单页面行 + 本路由 `meta.permission` | 侧栏显形 / 前端守卫（后端零引用） |
 * | `business:erp:inventory-transit:list` | V0421 按钮行 | 明细 / 汇总两个接口的框架闸 |
 * | `business:erp:inventory-transit:audit` | V0422 按钮行 | 对账接口（M2-05）+ **第三个 Tab 的显隐** |
 *
 * 没有 `:audit` 的角色**看不到「在途对账」Tab**，但页面照常能开（**不做整页 403**）：
 * 对账是出结论的授权面，看数的人不一定该出结论。
 *
 * 403 时页面给出点名缺哪个码的人话（见 `requestErrorMessage` / `reconcileErrorMessage`），而不是「无权限」四个字。
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
 * 本页因此没有任何写按钮 —— 在途台账是派生账，手改它等于伪造账；对账更是纯读（后端一行写 SQL 都没有）。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { useRouter } from 'vue-router';

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
  fetchTransitReconcile,
  fetchTransitSummary,
  type TransitQuery,
  type TransitReconcile,
  type TransitReconcileRow,
  type TransitRow,
  type TransitSummary,
  type TransitSummaryRow,
} from '@/api/business/erp/inventory-transit';
import { hasPermission } from '@/utils/flow-assignee-guard';

import {
  PERM_TRANSIT_AUDIT,
  RECONCILE_TAB,
  TRANSIT_PAGE_SIZE,
  TRANSIT_STATUS_OPTIONS,
  TRANSIT_TABS,
  ageText,
  ageTheme,
  buildReconcileQuery,
  buildSummaryQuery,
  buildTransitQuery,
  customerText,
  detailEmptyText,
  diffText,
  emptyReconcileFilter,
  emptySummaryFilter,
  emptyTransitFilter,
  formatQty,
  isPositiveQty,
  isZeroQty,
  issueText,
  issueTheme,
  materialText,
  reconcileBanner as reconcileBannerOf,
  reconcileEmptyText as reconcileEmptyTextOf,
  reconcileErrorMessage,
  reconcileResultText,
  reconcileResultTheme,
  reconcileRowKey,
  reconcileTruncatedNote as reconcileTruncatedNoteOf,
  reconcileWarehouseText,
  requestErrorMessage,
  statusLabel,
  statusTheme,
  summaryEmptyText,
  truncatedNote,
  warehouseText,
  type TransitFilterState,
  type TransitReconcileFilterState,
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

// ==================== Tab3：在途对账（M2-07b） ====================

/**
 * 第三个 Tab 的显隐：**只按对账码**（`business:erp:inventory-transit:audit`）判。
 *
 * 没有该码的角色看不到这个 Tab，但页面照常能开 —— 对账是出结论的授权面，
 * 不该把整个在途页 403 掉（看数的人和出结论的人可以不是同一个角色）。
 */
const canAudit = computed(() => hasPermission(PERM_TRANSIT_AUDIT));

/** 发货单读码：没有它时「发货单号」退化成纯文本（点了只会是一页 403） */
const canOpenShipment = computed(() => hasPermission('business:erp:shipment:list'));

const router = useRouter();

const reconcileFilter = reactive<TransitReconcileFilterState>(emptyReconcileFilter());
const reconcile = ref<TransitReconcile | null>(null);
const reconcileLoading = ref(false);
const reconcileError = ref('');
const reconcileLoaded = ref(false);

/** 行键 = 发货单 × 物料（后端 `ComboRow` 的 GROUP BY 就是这两列，一组一行） */
const reconcileRows = computed<Array<TransitReconcileRow & { __key: string }>>(() =>
  (reconcile.value?.rows ?? []).map((row) => ({ ...row, __key: reconcileRowKey(row) })),
);

/** 对账列（三侧数量 + 差额 + 结论；宽度合计 ≈1060，1440 笔记本屏不出现横向滚动） */
const reconcileColumns = [
  // 发货日期并进「发货单号」的下行：独立一列要 120px，挤掉的正是「结论」列（那是最该看见的一列）
  { colKey: 'ship_no', title: '发货单号 / 发货日期', width: 170 },
  { colKey: 'material', title: '物料', width: 170 },
  { colKey: 'warehouse', title: '发货仓库', width: 120 },
  { colKey: 'item_qty', title: '发货总量', width: 85, align: 'right' as const },
  { colKey: 'received_qty', title: '已出', width: 75, align: 'right' as const },
  { colKey: 'qty_in_transit', title: '在途', width: 75, align: 'right' as const },
  { colKey: 'diff', title: '差异量', width: 85, align: 'right' as const },
  { colKey: 'issues', title: '结论', minWidth: 280 },
];

const reconcileEmpty = computed(() => reconcileEmptyTextOf(reconcile.value, reconcileFilter));
const reconcileBanner = computed(() => reconcileBannerOf(reconcile.value));
const reconcileTruncatedHint = computed(() => reconcileTruncatedNoteOf(reconcile.value));

/** 日期区间写回（`t-date-range-picker` 可能给 `null`，一律归成空数组） */
function setReconcileDateRange(value: unknown): void {
  reconcileFilter.date_range = Array.isArray(value) ? value.map(String) : [];
}

/**
 * 跑一次对账（`GET /reconcile`）。
 *
 * 三侧数量、差额、`totals.*` 全部是后端给的字段，这里只做「赋值」——
 * 页面不做任何加减（单测断言页面源码里不出现 `reduce(`）。
 */
async function runReconcile(): Promise<void> {
  reconcileLoading.value = true;
  reconcileError.value = '';
  try {
    reconcile.value = await fetchTransitReconcile(buildReconcileQuery(reconcileFilter));
    reconcileLoaded.value = true;
  } catch (err) {
    reconcileError.value = reconcileErrorMessage(err);
  } finally {
    reconcileLoading.value = false;
  }
}

/** 清筛选并重跑 */
function resetReconcile(): void {
  Object.assign(reconcileFilter, emptyReconcileFilter());
  void runReconcile();
}

/**
 * 点发货单号 → 发货单页并**直接打开那张单的详情抽屉**。
 *
 * 走 query 参数（`?shipment_id=`）而不是「跳过去让用户自己找」：发货单页的
 * `onMounted` 读这个参数后 `openDetailById`（那张卡里已接）。
 */
function openShipment(row: TransitReconcileRow): void {
  void router.push({ path: '/business/erp/shipment', query: { shipment_id: row.shipment_id } });
}

/** 对账 Tab 也是 `lazy`：第一次切过去才跑（全量扫描上限 2000 组，别在首屏白跑） */
watch(activeTab, (tab) => {
  if (tab === RECONCILE_TAB.key && !reconcileLoaded.value && !reconcileLoading.value) {
    void runReconcile();
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

.transit-reconcile {
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 4px;
}

/* 「连对平的组一起看」：开关 + 文案包成一个 inline-flex，别被工具行换行拆开 */
.transit-check {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  white-space: nowrap;
}

.transit-check__text {
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

/* 结论列：判定 tag + 一句后端文案（文案不是词条，必须能换行） */
.transit-reconcile__issues {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  flex-wrap: wrap;
}

.transit-reconcile__issue-text {
  flex: 1 1 180px;
  font-size: 12px;
  line-height: 1.7;
  color: var(--td-text-color-secondary);
  word-break: break-word;
}

.transit-reconcile__issue-text[data-severity='danger'] {
  color: var(--td-error-color);
}

.transit-reconcile__issue-text[data-severity='warning'] {
  color: var(--td-warning-color);
}

.transit-reconcile__deleted {
  margin-left: 6px;
}

/* 工具栏行用 shell.less 里的 .tool-row（零壳类声明：页面不重新定义壳类） */

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

.num--danger {
  color: var(--td-error-color);
}

.transit-kpi__value--zero {
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
