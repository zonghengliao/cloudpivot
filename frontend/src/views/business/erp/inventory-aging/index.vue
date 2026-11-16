<template>
  <div class="aging-page">
    <t-card :bordered="false" class="aging-card">
      <!-- 主数据下拉缺读权限时给一句人话：列表本身不受影响，别让人以为「页面坏了」 -->
      <t-alert
        v-if="masterGaps.length"
        theme="info"
        class="aging-alert"
        data-testid="aging-master-gap"
        :message="`以下筛选下拉不可用（缺主数据读权限，查询本身不受影响）：${masterGaps.join(' / ')}`"
      />

      <!-- 一个取数码都没有：页面能进（有页面显形码）但三个 Tab 全 403 —— 说清缺什么 -->
      <t-alert
        v-if="!canReport && !canSlow && !canLayers"
        theme="warning"
        class="aging-alert"
        data-testid="aging-no-perm"
        :message="`当前账号没有库龄任何取数权限（${PERM_AGING_REPORT} / ${PERM_AGING_SLOW_MOVING} / ${PERM_AGING_LIST}），请让管理员在角色授权里勾上对应按钮行`"
      />

      <t-tabs v-if="canReport || canSlow || canLayers" v-model="activeTab" data-testid="aging-tabs">
        <!-- ==================== Tab 1：库龄分析（分段报表） ==================== -->
        <t-tab-panel v-if="canReport" :value="AGING_TABS[0].key" :label="AGING_TABS[0].label">
          <div class="aging-tool">
            <span class="aging-tool__label">基准日</span>
            <t-date-picker
              v-model="reportFilter.as_of"
              class="aging-tool__date"
              value-type="YYYY-MM-DD"
              allow-input
              clearable
              placeholder="留空 = 今天（CST）"
              data-testid="aging-report-asof"
            />
            <span class="aging-tool__label">物料</span>
            <t-select
              v-model="reportFilter.material_id"
              class="aging-tool__select"
              :options="materialOptions"
              filterable
              clearable
              placeholder="全部物料"
              data-testid="aging-report-material"
            />
            <span class="aging-tool__label">仓库</span>
            <t-select
              v-model="reportFilter.warehouse_id"
              class="aging-tool__select aging-tool__select--narrow"
              :options="warehouseOptions"
              filterable
              clearable
              placeholder="全部仓库"
              data-testid="aging-report-warehouse"
            />
            <t-button theme="primary" :loading="reportLoading" data-testid="aging-report-search" @click="loadReport()">
              查询
            </t-button>
          </div>

          <t-alert v-if="reportError" theme="error" class="aging-alert" data-testid="aging-report-error">
            <template #message>{{ reportError }}</template>
            <template #operation>
              <t-button size="small" variant="outline" @click="loadReport()">重试</t-button>
            </template>
          </t-alert>

          <div v-if="report" class="aging-head" data-testid="aging-report-head">
            <h2 class="aging-head__title">{{ report.report_name }}</h2>
            <div class="aging-head__meta">
              <span data-testid="aging-report-asof">
                基准日：{{ report.as_of }}（{{ asOfSourceText(report.as_of_source) }}）
              </span>
              <span>口径：FIFO 批次库龄，层日期取来源单据业务日期（非过账时间）</span>
            </div>
          </div>

          <div v-if="report" class="aging-kpi" data-testid="aging-report-kpi">
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">结存数量合计</span>
              <b class="aging-kpi__value" data-testid="aging-report-qty-total">{{ formatQty(report.qty_total) }}</b>
            </div>
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">参考金额合计</span>
              <b class="aging-kpi__value" data-testid="aging-report-amount-total">{{ formatMoney(report.amount_total) }}</b>
            </div>
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">库龄层数</span>
              <b class="aging-kpi__value" data-testid="aging-report-layers">{{ report.layer_count }}</b>
            </div>
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">物料 / 仓库</span>
              <b class="aging-kpi__value" data-testid="aging-report-counts">
                {{ report.material_count }} / {{ report.warehouse_count }}
              </b>
            </div>
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">物料 × 仓库 行数</span>
              <b class="aging-kpi__value" data-testid="aging-report-rows">{{ report.row_count }}</b>
            </div>
          </div>

          <t-alert
            v-if="report && report.rows_truncated"
            theme="warning"
            class="aging-alert"
            data-testid="aging-report-truncated"
            :message="truncatedNote(report.rows_truncated, report.row_count, report.row_limit)"
          />

          <t-loading :loading="reportLoading" size="small">
            <div v-if="report" class="aging-body" data-testid="aging-report-table">
              <table class="aging-table">
                <thead>
                  <tr>
                    <th class="aging-table__name">物料</th>
                    <th class="aging-table__name">仓库</th>
                    <th class="aging-table__num">层数</th>
                    <th v-for="bucket in report.buckets" :key="bucket.key" class="aging-table__num">
                      {{ bucket.label }}
                    </th>
                    <th class="aging-table__num aging-table__num--total">结存数量</th>
                    <th class="aging-table__num">参考金额</th>
                    <th class="aging-table__num">最老入库</th>
                    <th class="aging-table__num">最大库龄</th>
                    <th class="aging-table__num">加权平均</th>
                    <th class="aging-table__name">操作</th>
                  </tr>
                </thead>
                <tbody>
                  <!-- 合计行：**取后端 summary**（截断前全量），不把 rows 自己加一遍 -->
                  <tr class="aging-row--total" data-testid="aging-report-total">
                    <td class="aging-table__name">合计（{{ report.material_count }} 种物料 / {{ report.warehouse_count }} 个仓库）</td>
                    <td class="aging-table__name">—</td>
                    <td class="aging-table__num">{{ report.layer_count }}</td>
                    <td
                      v-for="bucket in report.buckets"
                      :key="bucket.key"
                      class="aging-table__num"
                      :data-testid="`aging-report-sum-${bucket.key}`"
                    >
                      <div>{{ formatQty(bucketCellOf(report.summary, bucket.key)?.qty) }}</div>
                      <div class="aging-sub">{{ formatMoney(bucketCellOf(report.summary, bucket.key)?.amount) }}</div>
                    </td>
                    <td class="aging-table__num aging-table__num--total" data-testid="aging-report-sum-qty">
                      {{ formatQty(report.qty_total) }}
                    </td>
                    <td class="aging-table__num">{{ formatMoney(report.amount_total) }}</td>
                    <td class="aging-table__num">—</td>
                    <td class="aging-table__num">—</td>
                    <td class="aging-table__num">—</td>
                    <td class="aging-table__name">—</td>
                  </tr>

                  <tr
                    v-for="(row, index) in report.rows"
                    :key="agingRowKey(row)"
                    class="aging-row"
                    :data-testid="`aging-report-row-${index}`"
                  >
                    <td class="aging-table__name">
                      <div :data-testid="`aging-report-material-${index}`">{{ materialText(row) }}</div>
                      <div class="aging-sub">{{ dash(row.spec) }}</div>
                    </td>
                    <td class="aging-table__name">{{ warehouseText(row) }}</td>
                    <td class="aging-table__num">{{ row.layer_count }}</td>
                    <td
                      v-for="bucket in report.buckets"
                      :key="bucket.key"
                      class="aging-table__num"
                      :class="{ 'aging-cell--zero': isZeroBucket(row.buckets, bucket.key) }"
                    >
                      <div>{{ formatQty(bucketCellOf(row.buckets, bucket.key)?.qty) }}</div>
                      <div class="aging-sub">{{ formatMoney(bucketCellOf(row.buckets, bucket.key)?.amount) }}</div>
                    </td>
                    <td class="aging-table__num aging-table__num--total">{{ formatQty(row.qty_total) }}</td>
                    <td class="aging-table__num">{{ formatMoney(row.amount_total) }}</td>
                    <td class="aging-table__num">{{ row.oldest_inbound_date }}</td>
                    <td class="aging-table__num" :class="`aging-cell--${ageTheme(row.max_age_days)}`">
                      {{ ageDaysText(row.max_age_days) }}
                    </td>
                    <td class="aging-table__num">{{ formatQty(row.avg_age_days) }}</td>
                    <td class="aging-table__name">
                      <t-link theme="primary" data-testid="aging-report-drill" @click="drillToLayers(row)">
                        批次层
                      </t-link>
                    </td>
                  </tr>

                  <tr v-if="!report.rows.length">
                    <td class="aging-empty" :colspan="report.buckets.length + 9" data-testid="aging-report-empty">
                      {{ reportEmptyText(report, isAgingFiltered(reportFilter)) }}
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </t-loading>

          <ul v-if="report && report.hints.length" class="aging-hints" data-testid="aging-report-hints">
            <li v-for="(hint, index) in report.hints" :key="index">{{ hint }}</li>
            <li>{{ reportTotalNote() }}</li>
          </ul>

          <div v-if="!report && !reportLoading && !reportError" class="aging-tip" data-testid="aging-report-tip">
            选好基准日 / 物料 / 仓库后点「查询」，看结存在 0-30 / 31-60 / 61-90 / 90 天以上 四档的分布
          </div>
        </t-tab-panel>

        <!-- ==================== Tab 2：呆滞清单 ==================== -->
        <t-tab-panel v-if="canSlow" :value="AGING_TABS[1].key" :label="AGING_TABS[1].label">
          <div class="aging-tool">
            <span class="aging-tool__label">基准日</span>
            <t-date-picker
              v-model="slowFilter.as_of"
              class="aging-tool__date"
              value-type="YYYY-MM-DD"
              allow-input
              clearable
              placeholder="留空 = 今天（CST）"
              data-testid="aging-slow-asof"
            />
            <span class="aging-tool__label">物料</span>
            <t-select
              v-model="slowFilter.material_id"
              class="aging-tool__select"
              :options="materialOptions"
              filterable
              clearable
              placeholder="全部物料"
              data-testid="aging-slow-material"
            />
            <span class="aging-tool__label">仓库</span>
            <t-select
              v-model="slowFilter.warehouse_id"
              class="aging-tool__select aging-tool__select--narrow"
              :options="warehouseOptions"
              filterable
              clearable
              placeholder="全部仓库"
              data-testid="aging-slow-warehouse"
            />
            <span class="aging-tool__label">阈值(天)</span>
            <t-input-number
              v-model="slowFilter.days"
              class="aging-tool__days"
              theme="normal"
              :min="1"
              :max="3650"
              :decimal-places="0"
              placeholder="留空 = 系统参数"
              data-testid="aging-slow-days"
            />
            <t-button theme="primary" :loading="slowLoading" data-testid="aging-slow-search" @click="loadSlow()">
              查询
            </t-button>
          </div>

          <t-alert v-if="slowError" theme="error" class="aging-alert" data-testid="aging-slow-error">
            <template #message>{{ slowError }}</template>
            <template #operation>
              <t-button size="small" variant="outline" @click="loadSlow()">重试</t-button>
            </template>
          </t-alert>

          <div v-if="slow" class="aging-head" data-testid="aging-slow-head">
            <h2 class="aging-head__title">{{ slow.report_name }}</h2>
            <div class="aging-head__meta">
              <span data-testid="aging-slow-asof-text">
                基准日：{{ slow.as_of }}（{{ asOfSourceText(slow.as_of_source) }}）
              </span>
              <span data-testid="aging-slow-threshold">{{ thresholdHint(slow) }}</span>
            </div>
          </div>

          <div v-if="slow" class="aging-kpi" data-testid="aging-slow-kpi">
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">呆滞格 / 扫描格</span>
              <b class="aging-kpi__value" data-testid="aging-slow-rows">
                {{ slow.summary.slow_row_count }} / {{ slow.summary.scanned_row_count }}
              </b>
            </div>
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">呆滞数量 / 结存数量</span>
              <b class="aging-kpi__value" data-testid="aging-slow-qty">
                {{ formatQty(slow.summary.qty_slow) }} / {{ formatQty(slow.summary.qty_on_hand) }}
              </b>
            </div>
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">呆滞金额 / 结存金额</span>
              <b class="aging-kpi__value" data-testid="aging-slow-amount">
                {{ formatMoney(slow.summary.amount_slow) }} / {{ formatMoney(slow.summary.amount_on_hand) }}
              </b>
            </div>
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">呆滞占比</span>
              <b
                class="aging-kpi__value"
                :class="`aging-cell--${slowRatioTheme(slow.summary.slow_ratio)}`"
                data-testid="aging-slow-ratio"
              >
                {{ formatRatioPercent(slow.summary.slow_ratio) }}
              </b>
            </div>
            <div class="aging-kpi__item">
              <span class="aging-kpi__label">呆滞层数 / 全部层数</span>
              <b class="aging-kpi__value" data-testid="aging-slow-layers">
                {{ slow.summary.slow_layer_count }} / {{ slow.summary.layer_count }}
              </b>
            </div>
          </div>

          <t-alert
            v-if="slow && slow.rows_truncated"
            theme="warning"
            class="aging-alert"
            data-testid="aging-slow-truncated"
            :message="truncatedNote(slow.rows_truncated, slow.row_count, slow.row_limit)"
          />

          <t-loading :loading="slowLoading" size="small">
            <div v-if="slow" class="aging-body" data-testid="aging-slow-table">
              <table class="aging-table">
                <thead>
                  <tr>
                    <th class="aging-table__name">物料</th>
                    <th class="aging-table__name">仓库</th>
                    <th class="aging-table__num">结存数量</th>
                    <th class="aging-table__num">参考金额</th>
                    <th class="aging-table__num">呆滞层数</th>
                    <th class="aging-table__num aging-table__num--total">呆滞数量</th>
                    <th class="aging-table__num">呆滞金额</th>
                    <th class="aging-table__num">呆滞占比</th>
                    <th class="aging-table__num">最老入库</th>
                    <th class="aging-table__num">最大库龄</th>
                    <th class="aging-table__name">操作</th>
                  </tr>
                </thead>
                <tbody>
                  <tr
                    v-for="(row, index) in slow.rows"
                    :key="agingRowKey(row)"
                    class="aging-row"
                    :data-testid="`aging-slow-row-${index}`"
                  >
                    <td class="aging-table__name">
                      <div :data-testid="`aging-slow-material-${index}`">{{ materialText(row) }}</div>
                      <div class="aging-sub">{{ dash(row.spec) }}</div>
                    </td>
                    <td class="aging-table__name">{{ warehouseText(row) }}</td>
                    <td class="aging-table__num">{{ formatQty(row.qty_on_hand) }}</td>
                    <td class="aging-table__num">{{ formatMoney(row.amount_on_hand) }}</td>
                    <td class="aging-table__num">{{ row.slow_layer_count }} / {{ row.layer_count }}</td>
                    <td class="aging-table__num aging-table__num--total">{{ formatQty(row.qty_slow) }}</td>
                    <td class="aging-table__num">{{ formatMoney(row.amount_slow) }}</td>
                    <td class="aging-table__num" :class="`aging-cell--${slowRatioTheme(row.slow_ratio)}`">
                      {{ formatRatioPercent(row.slow_ratio) }}
                    </td>
                    <td class="aging-table__num">{{ row.oldest_inbound_date }}</td>
                    <td class="aging-table__num" :class="`aging-cell--${ageTheme(row.max_age_days)}`">
                      {{ ageDaysText(row.max_age_days) }}
                    </td>
                    <td class="aging-table__name">
                      <t-link theme="primary" data-testid="aging-slow-drill" @click="drillToLayers(row)">
                        批次层
                      </t-link>
                    </td>
                  </tr>

                  <tr v-if="!slow.rows.length">
                    <td class="aging-empty" colspan="11" data-testid="aging-slow-empty">
                      {{ slowEmptyText(slow, isAgingFiltered(slowFilter)) }}
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </t-loading>

          <ul v-if="slow && slow.hints.length" class="aging-hints" data-testid="aging-slow-hints">
            <li v-for="(hint, index) in slow.hints" :key="index">{{ hint }}</li>
            <li>{{ reportTotalNote() }}</li>
          </ul>

          <div v-if="!slow && !slowLoading && !slowError" class="aging-tip" data-testid="aging-slow-tip">
            点「查询」列出「库龄超过 N 天且仍有剩余」的 物料 × 仓库；N 留空则按系统参数
            {{ SLOW_MOVING_CONFIG_KEY }}（缺省 {{ SLOW_MOVING_DEFAULT_DAYS }} 天）
          </div>
        </t-tab-panel>

        <!-- ==================== Tab 3：批次层明细（一层一行） ==================== -->
        <t-tab-panel v-if="canLayers" :value="AGING_TABS[2].key" :label="AGING_TABS[2].label">
          <ListPage
            ref="layerListRef"
            title="批次层明细"
            testid="aging-layer"
            table-testid="aging-layer-table"
            meta-testid="aging-layer-meta"
            meta-unit="层"
            btn-prefix="aging-layer"
            row-key="layer_id"
            :columns="layerColumns"
            :filters="layerFilters"
            :filter-state="layerFilter"
            :fetcher="layerFetcher"
            :query-builder="layerQueryBuilder"
            :page-size="AGING_PAGE_SIZE"
          >
            <template #filter-as_of="{ value, set }">
              <t-date-picker
                :model-value="value"
                class="aging-tool__date"
                value-type="YYYY-MM-DD"
                allow-input
                clearable
                placeholder="基准日（留空=今天）"
                data-testid="aging-layer-asof"
                @update:model-value="applyLayerAsOf(set, $event)"
              />
            </template>

            <template #filter-include_consumed="{ value, set }">
              <t-switch
                :model-value="Boolean(value)"
                class="aging-tool__switch"
                data-testid="aging-layer-consumed"
                @change="applyLayerConsumed(set, $event)"
              />
              <span class="aging-tool__hint">含已耗竭</span>
            </template>

            <!-- 库龄区间写反时要当场说清哪个框写反了（后端也会 400，但文案看不到是哪个） -->
            <template #filter-footer>
              <t-alert
                v-if="layerRangeError"
                theme="warning"
                :message="layerRangeError"
                data-testid="aging-layer-range-error"
              />
            </template>

            <template #head-extra>
              <span class="aging-tip-inline">
                层 = 一笔入库流水；剩余数量由后端按 FIFO 摊回（V0424 的库函数），本页只读。
                要「哪格压了多久」的汇总看「库龄分析」Tab。
              </span>
            </template>

            <template #material="{ row }">
              <div class="cell-stack">
                <span class="cell-stack__main">{{ materialText(row) }}</span>
                <span class="cell-stack__sub">{{ dash(row.spec) }}</span>
              </div>
            </template>

            <template #warehouse="{ row }">{{ warehouseText(row) }}</template>

            <template #doc_no="{ row }">
              <div class="cell-stack">
                <span class="cell-stack__main mono">{{ dash(row.doc_no) }}</span>
                <span class="cell-stack__sub">{{ docTypeText(row.doc_type) }} · 第 {{ row.line_no }} 行</span>
              </div>
            </template>

            <template #biz_type="{ row }">{{ bizTypeText(row.biz_type) }}</template>

            <template #age_days="{ row }">
              <span :class="`aging-cell--${ageTheme(row.age_days)}`">{{ ageDaysText(row.age_days) }}</span>
            </template>

            <template #qty_in="{ row }">{{ formatQty(row.qty_in) }}</template>
            <template #qty_consumed="{ row }">{{ formatQty(row.qty_consumed) }}</template>
            <template #qty_remaining="{ row }">
              <b>{{ formatQty(row.qty_remaining) }}</b>
            </template>
            <template #unit_cost="{ row }">{{ formatMoney(row.unit_cost) }}</template>
            <template #amount_remaining="{ row }">{{ formatMoney(row.amount_remaining) }}</template>

            <template #empty="{ filtered }">
              <div class="aging-empty-inline" data-testid="aging-layer-empty">{{ layerEmptyText(filtered) }}</div>
            </template>
          </ListPage>
        </t-tab-panel>
      </t-tabs>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 库龄页（M2-12）——「库龄分析 + 呆滞清单」两个视图，外加钻到层的批次层明细。
 *
 * # 三个 Tab 共用一本账
 *
 * | Tab | 接口 | 权限码 | 粒度 |
 * |-----|------|--------|------|
 * | 库龄分析 | `GET …/inventory-aging/report`（M2-09） | `…:inventory-aging:report` | 物料 × 仓库 × 四档 |
 * | 呆滞清单 | `GET …/inventory-aging/slow-moving`（M2-10） | `…:inventory-aging:slow-moving` | 物料 × 仓库（只留呆滞） |
 * | 批次层明细 | `GET …/inventory-aging/page`（M2-08） | `…:inventory-aging:list` | 一层一行 |
 *
 * 三者背后是同一个库函数 `biz_inventory_aging_layers`（V0424），页面**不做任何账**：
 * 档位边界、FIFO 摊回、呆滞判定、各档合计、占比全由后端算好，前端只排版 + 格式化。
 *
 * # 页面显形码与三个取数码分开
 *
 * 侧栏 / 路由用 `business:erp:inventory-aging:page`（V0427 页面行，后端零引用）；
 * 三个接口各自判自己的按钮码。所以本页按码决定**渲染哪几个 Tab**：
 * 只授报表码的角色看不到呆滞与明细 Tab（而不是点进去吃 403）。
 *
 * # 基准日为什么留空
 *
 * `as_of` 留空 = 后端取「今天（CST）」。前端**不自己算今天**：浏览器时区差一小时就会让
 * 「今天入库」变成库龄 1 天（后端 `today_cst` 是业务口径）。页头显示的是后端回显的
 * `as_of` 与 `as_of_source`（`QUERY` = 手填、`TODAY` = 今天）。
 *
 * # 合计只认后端
 *
 * `report.summary` / `slow.summary` 都是**截断前**算的全量；页面把 `rows` 自己加一遍
 * 就是第二套账（行被截断时必然算小）。`truncatedNote` 在表头上明写「合计仍是全量」。
 */
import { computed, nextTick, onMounted, reactive, ref, watch } from 'vue';

import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import {
  fetchMaterialPage,
  fetchWarehousePage,
  type MaterialRow,
  type Warehouse,
} from '@/api/business/erp';
import {
  fetchAgingLayers,
  fetchAgingReport,
  fetchSlowMoving,
  type AgingLayerQuery,
  type AgingLayerRow,
  type InventoryAgingReport,
  type SlowMovingReport,
  type SlowMovingRow,
} from '@/api/business/erp/inventory-aging';
import { hasPermission } from '@/utils/flow-assignee-guard';
import { dash, formatMoney } from '../shared/money';

import {
  AGING_PAGE_SIZE,
  AGING_TABS,
  PERM_AGING_LIST,
  PERM_AGING_REPORT,
  PERM_AGING_SLOW_MOVING,
  SLOW_MOVING_CONFIG_KEY,
  SLOW_MOVING_DEFAULT_DAYS,
  ageDaysText,
  ageTheme,
  ageRangeOf,
  agingRowKey,
  asOfSourceText,
  bizTypeText,
  bucketCellOf,
  buildLayerQuery,
  buildReportQuery,
  buildSlowQuery,
  docTypeText,
  drillLayerFilter,
  emptyAgingFilter,
  emptyLayerFilter,
  emptySlowFilter,
  formatQty,
  formatRatioPercent,
  isAgingFiltered,
  layerEmptyText,
  materialText,
  reportEmptyText,
  reportTotalNote,
  requestErrorMessage,
  slowEmptyText,
  slowRatioTheme,
  thresholdHint,
  truncatedNote,
  warehouseText,
  type AgingFilterState,
  type AgingTabKey,
  type LayerFilterState,
  type SlowFilterState,
} from './aging-logic';

// ==================== 权限（决定渲染哪几个 Tab） ====================

const canReport = computed(() => hasPermission(PERM_AGING_REPORT));
const canSlow = computed(() => hasPermission(PERM_AGING_SLOW_MOVING));
const canLayers = computed(() => hasPermission(PERM_AGING_LIST));

/** 首屏落在「第一个有码的 Tab」上；一个都没有时随便落在第一个（面板都不渲染，只看提示条） */
const activeTab = ref<AgingTabKey>(
  canReport.value ? AGING_TABS[0].key : canSlow.value ? AGING_TABS[1].key : AGING_TABS[2].key,
);

// ==================== 主数据下拉（能拉就拉） ====================

const materials = ref<MaterialRow[]>([]);
const warehouses = ref<Warehouse[]>([]);
/** 拉不到的主数据（缺权限 / 接口错）：给一句人话，不让下拉静默变空 */
const masterGaps = ref<string[]>([]);

const materialOptions = computed(() =>
  materials.value.map((item) => ({ label: `${item.material_code} ${item.material_name}`, value: item.id })),
);

const warehouseOptions = computed(() =>
  warehouses.value.map((item) => ({ label: `${item.warehouse_code} ${item.warehouse_name}`, value: item.id })),
);

/** 主数据下拉：缺读权限就不发请求（发了必然 403，还会让全局拦截器弹一次错） */
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

  await Promise.all(tasks);
  masterGaps.value = gaps;
}

// ==================== Tab 1：库龄分析 ====================

const reportFilter = reactive<AgingFilterState>(emptyAgingFilter());
const report = ref<InventoryAgingReport | null>(null);
const reportLoading = ref(false);
const reportError = ref('');

/** 一格都是 0 的档不涂灰（后端缺档给 0，0 是「这一档真没有货」） */
function isZeroBucket(cells: InventoryAgingReport['summary'], key: string): boolean {
  const cellQty = bucketCellOf(cells, key)?.qty;
  return cellQty === undefined || Number(cellQty) === 0;
}

async function loadReport(): Promise<void> {
  if (!canReport.value) {
    return;
  }
  reportLoading.value = true;
  reportError.value = '';
  try {
    report.value = await fetchAgingReport(buildReportQuery(reportFilter));
  } catch (error) {
    report.value = null;
    reportError.value = requestErrorMessage(error);
  } finally {
    reportLoading.value = false;
  }
}

// ==================== Tab 2：呆滞清单 ====================

const slowFilter = reactive<SlowFilterState>(emptySlowFilter());
const slow = ref<SlowMovingReport | null>(null);
const slowLoading = ref(false);
const slowError = ref('');

async function loadSlow(): Promise<void> {
  if (!canSlow.value) {
    return;
  }
  slowLoading.value = true;
  slowError.value = '';
  try {
    slow.value = await fetchSlowMoving(buildSlowQuery(slowFilter));
  } catch (error) {
    slow.value = null;
    slowError.value = requestErrorMessage(error);
  } finally {
    slowLoading.value = false;
  }
}

// ==================== Tab 3：批次层明细 ====================

const layerListRef = ref<InstanceType<typeof ListPage> | null>(null);
const layerFilter = reactive<LayerFilterState>(emptyLayerFilter());
/** 区间写反时**不发请求**（后端也会 400，但文案看不出是哪个框写反了） */
const layerRangeError = computed(() => ageRangeOf(layerFilter.min_age_days, layerFilter.max_age_days).error ?? '');

/** 自定义筛选控件写回（`ListPage` 的 `#filter-<key>` 槽给 `set`，这里只做类型/空值归一） */
function applyLayerAsOf(set: (value: unknown) => void, value: unknown): void {
  set(value ?? '');
}

function applyLayerConsumed(set: (value: unknown) => void, value: unknown): void {
  set(Boolean(value));
}

const layerColumns: ColumnDef[] = [
  { colKey: 'material', title: '物料', minWidth: 220 },
  { colKey: 'warehouse', title: '仓库', width: 160 },
  { colKey: 'lot_no', title: '批号', width: 150, ellipsis: true },
  { colKey: 'doc_no', title: '来源单据', width: 190 },
  { colKey: 'biz_type', title: '业务类型', width: 120 },
  { colKey: 'inbound_date', title: '入库日期', width: 120 },
  { colKey: 'age_days', title: '库龄(天)', width: 110, align: 'center' },
  { colKey: 'qty_in', title: '入库数量', width: 110, align: 'right' },
  { colKey: 'qty_consumed', title: '已耗竭', width: 110, align: 'right' },
  { colKey: 'qty_remaining', title: '剩余数量', width: 110, align: 'right' },
  { colKey: 'unit_cost', title: '层单价', width: 110, align: 'right' },
  { colKey: 'amount_remaining', title: '参考金额', width: 120, align: 'right' },
];

/**
 * 批次层筛选项。
 *
 * 两个自定义控件（基准日 `as_of` / 开关 `include_consumed`）走插槽：`ListPage` 只认
 * keyword / select / number / date-range，单日期与开关页面自绘。
 */
const layerFilters = computed<FilterDef[]>(() => [
  {
    key: 'material_id',
    type: 'select',
    placeholder: '物料（全部）',
    width: 220,
    options: materialOptions.value,
    filterable: true,
    testid: 'aging-layer-material',
  },
  {
    key: 'warehouse_id',
    type: 'select',
    placeholder: '仓库（全部）',
    width: 190,
    options: warehouseOptions.value,
    filterable: true,
    testid: 'aging-layer-warehouse',
  },
  { key: 'lot_no', type: 'keyword', placeholder: '批号（精确）', width: 150, testid: 'aging-layer-lot' },
  { key: 'min_age_days', type: 'number', placeholder: '最小库龄', width: 120, testid: 'aging-layer-min-age' },
  { key: 'max_age_days', type: 'number', placeholder: '最大库龄', width: 120, testid: 'aging-layer-max-age' },
  { key: 'as_of', type: 'custom', defaultValue: '', testid: 'aging-layer-asof' },
  { key: 'include_consumed', type: 'custom', defaultValue: false, testid: 'aging-layer-consumed' },
]);

const layerFetcher: ListFetcher<AgingLayerRow> = (query) => fetchAgingLayers(query as AgingLayerQuery);

const layerQueryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const filterState = state as LayerFilterState;
  // 区间写反：**不发请求**（`ListPage` 的 load 里 try/catch 会把它显示成错误态，
  // 比后端 400 的「min_age_days 不能…」更贴近用户刚才填的两个框）
  const rangeError = ageRangeOf(filterState.min_age_days, filterState.max_age_days).error;
  if (rangeError) {
    throw new Error(rangeError);
  }
  return buildLayerQuery(filterState, page, pageSize) as unknown as Record<string, unknown>;
};

/**
 * 从报表 / 呆滞行钻到批次层：把 物料 × 仓库 + 该表的基准日 带过去。
 *
 * Tab 是懒挂载的（第一次切过去才渲染 `ListPage`），所以用 `pendingLayerSearch` 记一笔，
 * 等面板挂上再触发 `search()` —— 直接 `layerListRef.value?.search()` 在挂载前是 `null`。
 */
const pendingLayerSearch = ref(false);

function drillToLayers(row: SlowMovingRow | InventoryAgingReport['rows'][number]): void {
  const asOf = report.value?.as_of ?? slow.value?.as_of ?? '';
  Object.assign(layerFilter, drillLayerFilter(row, asOf));
  activeTab.value = AGING_TABS[2].key;
  pendingLayerSearch.value = true;
}

watch(activeTab, async (tab) => {
  if (tab === AGING_TABS[2].key && pendingLayerSearch.value) {
    pendingLayerSearch.value = false;
    await nextTick();
    layerListRef.value?.search();
  }
});

/** 区间写反时在筛选条下面提示（`ListPage` 的 `#filter-footer` 槽）—— 用 computed 一次说清 */

// ==================== 首屏 ====================

onMounted(async () => {
  await loadMasterData();
  // 只加载当前展示的那个 Tab 的数据（切 Tab 时按需拉，见下面的 watch）
  if (canReport.value) {
    await loadReport();
  }
  if (!canReport.value && canSlow.value) {
    await loadSlow();
  }
});

/** 切到还没查过的 Tab 时自动拉一次（用户不必先点「查询」才看到东西） */
watch(activeTab, (tab) => {
  if (tab === AGING_TABS[1].key && !slow.value && !slowLoading.value && !slowError.value) {
    void loadSlow();
  }
  if (tab === AGING_TABS[0].key && !report.value && !reportLoading.value && !reportError.value) {
    void loadReport();
  }
});
</script>

<style scoped>
.aging-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.aging-card {
  padding: 4px 4px 8px;
}

.aging-alert {
  margin-bottom: 12px;
}

.aging-tool {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  padding: 4px 0 12px;
}

.aging-tool__label {
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.aging-tool__date {
  width: 170px;
}

.aging-tool__select {
  width: 220px;
}

.aging-tool__select--narrow {
  width: 190px;
}

.aging-tool__days {
  width: 150px;
}

.aging-tool__switch {
  margin-right: 6px;
}

.aging-tool__hint {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  margin-right: 8px;
}

.aging-tip-inline {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.aging-head {
  text-align: center;
  margin: 4px 0 12px;
}

.aging-head__title {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 600;
  letter-spacing: 2px;
}

.aging-head__meta {
  display: flex;
  justify-content: center;
  gap: 20px;
  flex-wrap: wrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.aging-kpi {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.aging-kpi__item {
  flex: 1 1 180px;
  padding: 10px 14px;
  border-radius: 6px;
  background: var(--td-bg-color-secondarycontainer);
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.aging-kpi__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.aging-kpi__value {
  font-size: 18px;
  font-weight: 600;
}

.aging-body {
  overflow-x: auto;
}

.aging-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.aging-table th,
.aging-table td {
  border: 1px solid var(--td-component-border);
  padding: 6px 10px;
  line-height: 1.6;
  vertical-align: top;
}

.aging-table thead th {
  background: var(--td-bg-color-secondarycontainer);
  font-weight: 600;
  text-align: center;
}

.aging-table__name {
  text-align: left;
}

.aging-table__num {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.aging-table__num--total {
  font-weight: 600;
}

.aging-row--total td {
  background: var(--td-bg-color-container-select, #f3f6fb);
  font-weight: 600;
}

.aging-sub {
  font-size: 11px;
  color: var(--td-text-color-placeholder);
  font-weight: 400;
}

.aging-cell--zero {
  color: var(--td-text-color-placeholder);
}

.aging-cell--danger {
  color: var(--td-error-color);
  font-weight: 600;
}

.aging-cell--warning {
  color: var(--td-warning-color);
  font-weight: 600;
}

.aging-cell--default {
  color: inherit;
}

.aging-empty {
  text-align: center;
  color: var(--td-text-color-placeholder);
  padding: 24px 0;
}

.aging-empty-inline {
  text-align: center;
  color: var(--td-text-color-placeholder);
  padding: 24px 0;
}

.aging-hints {
  margin: 12px 0 0;
  padding-left: 18px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.8;
}

.aging-tip {
  color: var(--td-text-color-placeholder);
  text-align: center;
  padding: 40px 0;
}

.cell-stack {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.cell-stack__main {
  font-weight: 500;
}

.cell-stack__sub {
  font-size: 11px;
  color: var(--td-text-color-secondary);
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}
</style>
