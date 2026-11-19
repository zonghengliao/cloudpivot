<!--
  成本还原页（M5-30【M5·成本】成本还原：前端页 · 还原明细）。

  一页两个页签，各是一条完整链路的前端落点：

  ```
  ┌ 页签 ① 还原明细 = 逐成本对象（一行一批）：还原前料工费 → 逐层卷积 → 还原后料工费
  │                展开行 = 三要素对照 + 逐层台账 + 半成品还原明细 + 怎么修
  └ 页签 ② 还原后报表 = 按车间 / 产品 / 对象 / 整表汇总的料工费占比与结构变化（pp）
  ```

  # 这两个页签回答的是同一个问题的两半

  「本期材料里那笔自制半成品，最后到底变成了多少外购料、多少人工、多少制造费用？」

  * **明细**回答「这一批是怎么拆的」：哪一层拨下去多少钱、哪一层卡住没拆完（还原不彻底）；
  * **报表**回答「放在车间 / 产品这个口径上看，料工费结构变了多少个百分点」——
    占比分母统一取还原前合计，所以材料 + 人工 + 制造费用 + 还原不彻底 ≈ 100.00%。

  # 数据从哪来（两条只读接口，零新增权限码）

  | 页签 | 接口 | 权限 |
  |---|---|---|
  | 还原明细 | `GET /api/business/erp/cost-restore-rollup`（M5-27） | `cost-object:list` |
  | 还原后报表 | `GET /api/business/erp/cost-restore-report`（M5-28） | `cost-object:list` |

  两条都**复用**成本对象的读码（看得到成本对象的人就该看到「这笔材料拆成了什么」）；
  本页**没有任何写动作**（还原是只读计算），所以不挂保存按钮、不判写码。

  # 前端不重算任何公式（本页最重要的口径）

  * 逐层卷积（材料里的半成品按所产批次完工结构递归拆开）唯一实现在
    `cost_restore_rollup_dto::restore_of`（M5-27）；页面只显示 `after` / `delta` / `flows[]` / `semis[]`；
  * 分组汇总与料工费占比唯一实现在 `cost_restore_report_dto::aggregate` / `group_view`（M5-28）；
  * 合计一律用后端 `totals`（服务端分页下，拿当前页加出来的合计翻页就变，
    而财务会拿它跟成本对象台账对）。

  # 三个必须原样转达、不许「美化」的点

  | 位置 | 事实 | 页面怎么画 |
  |---|---|---|
  | `share_sum` | 四项各四舍五入到 2 位，合计可能是 `100.01`（M5-29 实测） | 容差 ±0.02（`shareSumTone`），**不硬判 100** |
  | `flows[]` | 第 2 层起是上一层「材料部分」往下拨的钱，**不是新增投入** | 逐层展示，**不相加**；列头与说明都写清 |
  | `state = partial` | 一部分金额停在半成品形态（缺结构 / 闭环 / 层数上限 / 没下一层） | 红 tag + 展开行逐条 `issues` + 「调大层数」提示 |

  # 三种 `null` 不许画成 0（会被读成「没有金额」）

  | 位置 | `null` 的含义 | 页面怎么画 |
  |---|---|---|
  | `row.item` / `row.finished_cost` | 本期还没算过这个成本对象 | 「本期还没算过」+ 一句「还原照做」 |
  | `*_share` / `*_pp` / `share_sum` | 分母为 0，占比无从谈起 | `—` |
  | `semis[].source` | 该节点没有结构来源（取不到结构，已计入不彻底） | `—` + `stopped` 标记 |

  # 与「在制品」页的区别（别以为是重复页面）

  `/business/erp/cost-wip` 看在制**余额**（期初 + 投入 − 转出 = 期末，钱滚到下个月了没有）；
  本页看**成本结构还原**（材料里的半成品最后变成了什么料工费）。前者是结账视角，
  后者是成本项目分析视角，两张表都对得上，但回答的问题不一样。
-->
<template>
  <div class="cr-page">
    <t-tabs v-model="activeTab" class="page-tabs" data-testid="cr-tabs" @change="onTabChange">
      <t-tab-panel value="detail" label="还原明细" data-testid="cr-tab-detail" />
      <t-tab-panel value="report" label="还原后报表" data-testid="cr-tab-report" />
    </t-tabs>

    <!-- 工具栏：两个页签共用期间 / 关键字 / 状态 / 层数（报表多一个维度） -->
    <t-card :bordered="false" class="cr-tool-card">
      <div class="tool-row tool-row--dense">
        <t-date-picker
          v-model="filter.period_code"
          class="cr-period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="会计期间（必选）"
          data-testid="cr-filter-period"
          @change="searchCurrent"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="对象号 / 生产订单号 / 产出批号 / 产出品"
          clearable
          data-testid="cr-filter-keyword"
          @enter="searchCurrent"
        />
        <t-select
          v-model="filter.state"
          class="cr-state"
          :options="stateOptions"
          placeholder="全部状态"
          data-testid="cr-filter-state"
          @change="searchCurrent"
        />
        <div class="cr-depth">
          <span class="cr-depth__label">层数</span>
          <t-input-number
            v-model="filter.max_depth"
            theme="normal"
            :min="MAX_DEPTH_MIN"
            :max="MAX_DEPTH_LIMIT"
            :step="1"
            data-testid="cr-filter-depth"
            @change="searchCurrent"
          />
        </div>
        <t-select
          v-if="activeTab === 'report'"
          v-model="filter.group_by"
          class="cr-group"
          :options="groupOptions"
          placeholder="汇总维度"
          data-testid="cr-filter-group"
          @change="searchCurrent"
        />
        <t-button theme="primary" :disabled="!periodReady" data-testid="cr-btn-search" @click="searchCurrent">
          查询
        </t-button>
        <t-button variant="outline" data-testid="cr-btn-reset" @click="resetFilter">重置</t-button>
        <span class="tool-row__spacer" />
        <span class="tool-row__hint">
          还原 = 材料里的自制半成品按其产出批次的结构逐层拆成料 / 工 / 费（总额一分不动，期间必填）
        </span>
      </div>
    </t-card>

    <t-alert
      v-if="!periodReady"
      theme="info"
      :message="PERIOD_REQUIRED_HINT"
      data-testid="cr-period-hint"
    />

    <t-alert v-else-if="scopeHint" theme="warning" :message="scopeHint" data-testid="cr-scope-hint">
      <template #operation>
        <t-button size="small" variant="outline" data-testid="cr-btn-to-cost-period" @click="goCostPeriod">
          去成本核算
        </t-button>
      </template>
    </t-alert>

    <!-- ==================================================================== -->
    <!-- ① 还原明细：一行一个成本对象，展开看逐层台账与半成品明细              -->
    <!-- ==================================================================== -->
    <template v-if="activeTab === 'detail'">
      <t-card v-if="rollupData && rollupData.total > 0" :bordered="false" class="cr-summary-card">
        <div class="panel-head">
          <div class="panel-head__main">
            <span class="panel-head__title">整表合计</span>
            <span class="panel-head__meta" data-testid="cr-detail-scope-meta">
              {{ rollupData.period_code }}
              <template v-if="rollupData.batch"> · 批次 {{ rollupData.batch.batch_no }}（{{ rollupData.batch.status }}）</template>
              <template v-else> · 无成本核算批次</template>
              <template v-if="rollupData.keyword"> · 关键字「{{ rollupData.keyword }}」</template>
            </span>
          </div>
        </div>

        <div class="stat-row" data-testid="cr-detail-stats">
          <div v-for="card in detailSummary" :key="card.key" class="stat-card" :data-testid="`cr-detail-stat-${card.key}`">
            <span class="stat-card__label">{{ card.label }}</span>
            <span class="stat-card__value" :class="valueClass(card.tone)">{{ card.value }}</span>
            <span class="stat-card__sub">{{ card.sub }}</span>
          </div>
        </div>

        <p class="cr-basis" data-testid="cr-detail-formula-restore">{{ noteText(rollupData.formula_restore) }}</p>
        <p class="cr-basis" data-testid="cr-detail-formula-unresolved">{{ noteText(rollupData.formula_unresolved) }}</p>
      </t-card>

      <ListPage
        ref="detailListRef"
        class="cr-list"
        title="还原明细"
        meta-unit="个"
        testid="cr-detail"
        table-testid="cr-detail-table"
        meta-testid="cr-detail-meta"
        btn-prefix="cr-detail-btn"
        row-key="object_id"
        expandable
        :table-slots="['expandedRow']"
        :exportable="false"
        :columns="detailColumns"
        :filters="[]"
        :fetcher="detailFetcher"
        :query-builder="detailQueryBuilder"
        :page-size="PAGE_SIZE"
      >
        <template #meta-extra>
          <span class="cr-muted">{{ detailMetaSuffix }}</span>
        </template>

        <template #object="{ row }">
          <div class="cr-object">
            <span class="cr-object__no">{{ row.object_no }}</span>
            <span class="cr-object__sub">{{ row.lot_no ? `批号 ${row.lot_no}` : '（无产出批号）' }}</span>
          </div>
        </template>

        <template #product="{ row }">
          <div class="cr-object">
            <span>{{ productText(row) }}</span>
            <span class="cr-object__sub">{{ row.workshop_dept_name || '（无车间）' }}</span>
          </div>
        </template>

        <template #before="{ row }">
          <div class="cr-object">
            <span class="mono">{{ moneyText(row.before.total) }}</span>
            <span class="cr-object__sub">
              料 {{ moneyText(row.before.material) }} · 工 {{ moneyText(row.before.labor) }} · 费 {{ moneyText(row.before.overhead) }}
            </span>
          </div>
        </template>

        <template #semi="{ row }">
          <div class="cr-object">
            <span class="mono">{{ pctText(row.semi_share) }}</span>
            <span class="cr-object__sub">半成品 {{ moneyText(row.semi_in_material) }}</span>
          </div>
        </template>

        <template #after="{ row }">
          <div class="cr-object">
            <span class="mono cr-strong">{{ moneyText(row.after.total) }}</span>
            <span class="cr-object__sub">
              料 {{ moneyText(row.after.material) }} · 工 {{ moneyText(row.after.labor) }} · 费 {{ moneyText(row.after.overhead) }}
            </span>
          </div>
        </template>

        <template #delta="{ row }">
          <div class="cr-object">
            <span class="mono" :class="row.delta.material.startsWith('-') ? 'cr-bad' : ''">
              料 {{ signedAmount(row.delta.material) }}
            </span>
            <span class="cr-object__sub">
              工 {{ signedAmount(row.delta.labor) }} · 费 {{ signedAmount(row.delta.overhead) }} · 合计 {{ signedAmount(row.delta.total) }}
            </span>
          </div>
        </template>

        <template #unresolved="{ row }">
          <span class="mono" :class="unresolvedTone(row) === 'danger' ? 'cr-bad' : 'cr-muted'">
            {{ moneyText(row.unresolved) }}
          </span>
        </template>

        <template #state="{ row }">
          <t-tag size="small" variant="light" :theme="stateTheme(row.state)">{{ stateLabelOf(row) }}</t-tag>
        </template>

        <template #depth="{ row }">
          <div class="cr-object cr-object--right">
            <span class="mono">{{ row.depth }} 层</span>
            <span class="cr-object__sub">{{ row.node_count }} 节点</span>
          </div>
        </template>

        <template #empty>
          <StateView
            state="empty"
            compact
            :title="detailEmptyText"
            description="换一个期间 / 关键字 / 状态，或先到「成本核算」把该期间算出来。"
            testid="cr-detail-empty"
          />
        </template>

        <!-- 展开行：三要素对照 + 逐层台账 + 半成品明细 + 怎么修 -->
        <template #expandedRow="{ row }">
          <div class="cr-detail" data-testid="cr-detail-expanded">
            <div class="cr-detail__cols">
              <div class="cr-detail__col">
                <div class="cr-detail__title">这一批是什么</div>
                <dl class="cr-facts">
                  <template v-for="fact in rowFacts(row)" :key="`f-${fact.label}`">
                    <dt>{{ fact.label }}</dt>
                    <dd :class="[fact.tone === 'danger' ? 'cr-bad' : '', 'mono']">{{ fact.value }}</dd>
                  </template>
                </dl>
              </div>
              <div class="cr-detail__col">
                <div class="cr-detail__title">还原前 → 还原后（差额由后端给）</div>
                <t-table
                  row-key="key"
                  size="small"
                  :data="elementLines(row)"
                  :columns="elementColumns"
                  data-testid="cr-detail-elements"
                >
                  <template #label="{ row: line }">
                    <span class="cr-strong">{{ line.label }}</span>
                  </template>
                  <template #before="{ row: line }">
                    <span class="mono cr-muted">{{ line.before }}</span>
                  </template>
                  <template #after="{ row: line }">
                    <span class="mono cr-strong">{{ line.after }}</span>
                  </template>
                  <template #delta="{ row: line }">
                    <span class="mono" :class="line.delta.startsWith('-') ? 'cr-bad' : line.delta === '0' ? 'cr-muted' : 'cr-ok'">
                      {{ line.delta }}
                    </span>
                  </template>
                </t-table>

                <t-alert
                  v-if="row.state === 'partial'"
                  theme="warning"
                  class="cr-alert"
                  :message="PARTIAL_HINT"
                  data-testid="cr-detail-partial-hint"
                />
              </div>
            </div>

            <div class="cr-detail__title">
              逐层台账（各层不能相加：第 2 层起是上一层「材料部分」往下拨的钱）
            </div>
            <t-table
              v-if="row.flows.length > 0"
              row-key="key"
              size="small"
              :data="flowLines(row.flows)"
              :columns="flowColumns"
              :bordered="true"
              data-testid="cr-detail-flows"
            >
              <template #level="{ row: flow }">
                <span class="cr-strong">{{ flow.levelText }}</span>
              </template>
              <template #nodes="{ row: flow }">
                <span class="mono cr-muted">{{ flow.nodes }}</span>
              </template>
              <template #imputed="{ row: flow }">
                <span class="mono cr-strong">{{ flow.imputed }}</span>
              </template>
              <template #purchased="{ row: flow }">
                <span class="mono">{{ flow.purchased }}</span>
              </template>
              <template #labor="{ row: flow }">
                <span class="mono">{{ flow.labor }}</span>
              </template>
              <template #overhead="{ row: flow }">
                <span class="mono">{{ flow.overhead }}</span>
              </template>
              <template #carried="{ row: flow }">
                <span class="mono">{{ flow.carriedDown }}</span>
              </template>
              <template #unresolved="{ row: flow }">
                <span class="mono" :class="flow.unresolved !== '0' ? 'cr-bad' : 'cr-muted'">{{ flow.unresolved }}</span>
              </template>
              <template #balance="{ row: flow }">
                <span class="mono" :class="flow.balanced ? 'cr-ok' : 'cr-bad'">{{ flow.balance }}</span>
              </template>
              <template #note="{ row: flow }">
                <span class="cr-muted cr-note">{{ flow.note }}</span>
              </template>
            </t-table>
            <p v-else class="cr-foot-note" data-testid="cr-detail-flows-empty">
              本期材料里没有需要拆的节点（材料全是外购料，或本期没有材料投入）：还原前后一致。
            </p>

            <div class="cr-detail__title">半成品还原明细（一行一个被拆掉的自制半成品节点）</div>
            <t-table
              v-if="row.semis.length > 0"
              row-key="key"
              size="small"
              :data="semiLines(row.semis)"
              :columns="semiColumns"
              data-testid="cr-detail-semis"
            >
              <template #material="{ row: semi }">
                <div class="cr-object">
                  <span>{{ semi.material }}</span>
                  <span class="cr-object__sub">{{ semi.stoppedText }}</span>
                </div>
              </template>
              <template #lot="{ row: semi }">
                <span class="mono">{{ semi.lotNo }}</span>
              </template>
              <template #imputed="{ row: semi }">
                <span class="mono cr-strong">{{ semi.imputed }}</span>
              </template>
              <template #materialCost="{ row: semi }">
                <span class="mono">{{ semi.materialCost }}</span>
              </template>
              <template #labor="{ row: semi }">
                <span class="mono">{{ semi.labor }}</span>
              </template>
              <template #overhead="{ row: semi }">
                <span class="mono">{{ semi.overhead }}</span>
              </template>
              <template #unresolved="{ row: semi }">
                <span class="mono" :class="semi.unresolved !== '0' ? 'cr-bad' : 'cr-muted'">{{ semi.unresolved }}</span>
              </template>
              <template #source="{ row: semi }">
                <span class="cr-muted">{{ semi.source }}</span>
              </template>
              <template #producer="{ row: semi }">
                <span class="mono cr-muted">{{ semi.producer }}</span>
              </template>
            </t-table>
            <p v-else class="cr-foot-note" data-testid="cr-detail-semis-empty">
              {{ row.state === 'no_semi' ? '本期材料里没有自制半成品（无需还原）。' : '没有拆到半成品节点：材料直接就是外购料。' }}
            </p>

            <template v-if="hintsOf(row).length > 0">
              <div class="cr-detail__title">这一行怎么理解 / 怎么修</div>
              <t-alert
                v-for="(hint, index) in hintsOf(row)"
                :key="`h-${index}`"
                :theme="row.state === 'partial' ? 'warning' : 'info'"
                class="cr-alert"
                :message="hint"
                data-testid="cr-detail-hint"
              />
            </template>
          </div>
        </template>

        <template #footer>
          <p v-if="detailBasis" class="cr-foot-note" data-testid="cr-detail-note">{{ detailBasis }}</p>
        </template>
      </ListPage>
    </template>

    <!-- ==================================================================== -->
    <!-- ② 还原后报表：按车间 / 产品 / 对象 / 整表汇总料工费占比与结构变化      -->
    <!-- ==================================================================== -->
    <template v-else>
      <t-card v-if="reportData" :bordered="false" class="cr-summary-card">
        <div class="panel-head">
          <div class="panel-head__main">
            <span class="panel-head__title">整表合计与结构变化</span>
            <span class="panel-head__meta" data-testid="cr-report-scope-meta">
              {{ reportData.period_code }} · 维度 {{ reportData.group_by_label }} · 状态 {{ reportData.state_label }}
              <template v-if="reportData.batch"> · 批次 {{ reportData.batch.batch_no }}（{{ reportData.batch.status }}）</template>
            </span>
          </div>
        </div>

        <div class="stat-row" data-testid="cr-report-stats">
          <div v-for="card in reportSummary" :key="card.key" class="stat-card" :data-testid="`cr-report-stat-${card.key}`">
            <span class="stat-card__label">{{ card.label }}</span>
            <span class="stat-card__value" :class="valueClass(card.tone)">{{ card.value }}</span>
            <span class="stat-card__sub">{{ card.sub }}</span>
          </div>
        </div>

        <p class="cr-basis" data-testid="cr-report-basis">{{ reportBasisText }}</p>
      </t-card>

      <ListPage
        ref="reportListRef"
        class="cr-list"
        title="还原后报表"
        meta-unit="组"
        testid="cr-report"
        table-testid="cr-report-table"
        meta-testid="cr-report-meta"
        btn-prefix="cr-report-btn"
        row-key="key"
        :exportable="false"
        :columns="reportColumns"
        :filters="[]"
        :fetcher="reportFetcher"
        :query-builder="reportQueryBuilder"
        :page-size="REPORT_PAGE_SIZE"
      >
        <template #meta-extra>
          <span class="cr-muted">{{ reportMetaSuffix }}</span>
        </template>

        <template #label="{ row }">
          <div class="cr-object">
            <span class="cr-strong">{{ row.label }}</span>
            <span class="cr-object__sub">{{ row.hint || row.dominant }}</span>
          </div>
        </template>

        <template #objects="{ row }">
          <t-tooltip :content="row.rowCounts">
            <span class="mono">{{ row.objectCount }}</span>
          </t-tooltip>
        </template>

        <template #before="{ row }">
          <div class="cr-object cr-object--right">
            <span class="mono">{{ row.beforeTotal }}</span>
            <span class="cr-object__sub">{{ row.beforeElements }}</span>
          </div>
        </template>

        <template #after="{ row }">
          <div class="cr-object cr-object--right">
            <span class="mono cr-strong">{{ row.afterTotal }}</span>
            <span class="cr-object__sub">{{ row.afterElements }}</span>
          </div>
        </template>

        <template #materialShare="{ row }">
          <div class="cr-object cr-object--right">
            <span class="mono">{{ row.materialShare }}</span>
            <span class="cr-object__sub" :class="row.materialPp.startsWith('-') ? 'cr-bad' : 'cr-ok'">{{ row.materialPp }}</span>
          </div>
        </template>

        <template #laborShare="{ row }">
          <div class="cr-object cr-object--right">
            <span class="mono">{{ row.laborShare }}</span>
            <span class="cr-object__sub" :class="row.laborPp.startsWith('-') ? 'cr-bad' : 'cr-ok'">{{ row.laborPp }}</span>
          </div>
        </template>

        <template #overheadShare="{ row }">
          <div class="cr-object cr-object--right">
            <span class="mono">{{ row.overheadShare }}</span>
            <span class="cr-object__sub" :class="row.overheadPp.startsWith('-') ? 'cr-bad' : 'cr-ok'">{{ row.overheadPp }}</span>
          </div>
        </template>

        <template #unresolved="{ row }">
          <div class="cr-object cr-object--right">
            <span class="mono" :class="row.unresolvedShare === '—' ? 'cr-muted' : 'cr-bad'">{{ row.unresolvedShare }}</span>
            <span class="cr-object__sub">不彻底</span>
          </div>
        </template>

        <template #shareSum="{ row }">
          <t-tooltip :content="shareSumText(row.shareRaw)">
            <span class="mono" :class="row.shareSumTone === 'off' ? 'cr-bad' : 'cr-muted'">{{ row.shareSum }}</span>
          </t-tooltip>
        </template>

        <template #check="{ row }">
          <t-tooltip :content="checkText(row.checkRaw)">
            <span class="mono" :class="row.checkTone === 'ok' ? 'cr-ok' : 'cr-bad'">{{ row.check }}</span>
          </t-tooltip>
        </template>

        <template #empty>
          <StateView
            state="empty"
            compact
            :title="reportEmpty"
            description="换一个维度 / 期间，或先看明细页签确认本期有可还原的成本对象。"
            testid="cr-report-empty"
          />
        </template>

        <template #footer>
          <p v-if="reportBasisText" class="cr-foot-note" data-testid="cr-report-note">{{ reportBasisText }}</p>
        </template>
      </ListPage>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';

import {
  MAX_DEPTH_LIMIT,
  MAX_DEPTH_MIN,
  PAGE_SIZE,
  REPORT_PAGE_SIZE,
  fetchRestoreReport,
  fetchRestoreRollup,
  type RestoreReportQuery,
  type RestoreReportRes,
  type RestoreRollupQuery,
  type RestoreRollupRes,
  type RestoreRow,
} from '@/api/business/erp/cost-restore';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, ListFetcher, ListQuery, ListQueryBuilder } from '@/components/ListPage/types';
import StateView from '@/components/StateView/index.vue';
import {
  PARTIAL_HINT,
  PERIOD_REQUIRED_HINT,
  buildReportQuery,
  buildRollupQuery,
  checkText,
  defaultRestoreFilter,
  elementLines,
  emptyTextOf,
  flowLines,
  groupLines,
  groupSelectOptions,
  hasPeriod,
  hintsOf,
  moneyText,
  noteText,
  pctText,
  productText,
  reportBasis,
  reportCards,
  reportEmptyText,
  rollupBasis,
  rowFacts,
  scopeHintOf,
  semiLines,
  shareSumText,
  signedAmount,
  stateLabelOf,
  stateSelectOptions,
  stateTheme,
  summaryCards,
  unresolvedTone,
  type GroupLine,
  type RestoreFilter,
  type SummaryCard,
} from './cost-restore-logic';

const router = useRouter();

/** 页签（`detail` 还原明细 / `report` 还原后报表） */
const activeTab = ref<'detail' | 'report'>('detail');

// ==================== 筛选（两个页签共用一份） ====================

const filter = reactive<RestoreFilter>(defaultRestoreFilter());

const stateOptions = stateSelectOptions();
const groupOptions = groupSelectOptions();

/** 期间是否可查（空期间**不发请求**，画引导条） */
const periodReady = computed(() => hasPeriod(filter));

function onTabChange(value: string | number): void {
  if (value === 'report') {
    searchReport();
  } else {
    searchDetail();
  }
}

function searchCurrent(): void {
  if (activeTab.value === 'report') {
    searchReport();
  } else {
    searchDetail();
  }
}

function resetFilter(): void {
  const next = defaultRestoreFilter();
  // 维度跟着页签走：在报表页重置不该把维度也丢掉
  Object.assign(filter, next, activeTab.value === 'report' ? { group_by: filter.group_by } : {});
  searchCurrent();
}

function goCostPeriod(): void {
  void router.push('/business/erp/cost-period');
}

/** 语义色 class（红 / 绿 / 无色） */
function valueClass(tone: string): string {
  if (tone === 'danger') {
    return 'stat-card__value--bad';
  }
  if (tone === 'success') {
    return 'stat-card__value--good';
  }
  if (tone === 'primary') {
    return 'stat-card__value--primary';
  }
  return '';
}

// ====================================================================================
// ① 还原明细
// ====================================================================================

const detailListRef = ref<InstanceType<typeof ListPage> | null>(null);
const rollupData = ref<RestoreRollupRes | null>(null);

/** 汇总卡片（直接用后端 totals，前端不累加） */
const detailSummary = computed<SummaryCard[]>(() => summaryCards(rollupData.value));

/** 顶部引导条（没有成本核算批次时说明「还原照做」） */
const scopeHint = computed(() => scopeHintOf(rollupData.value));

/** 合计旁边的补充说明（不彻底行数 / 层数提示） */
const detailMetaSuffix = computed(() => {
  const totals = rollupData.value?.totals;
  if (!totals || totals.row_count === 0) {
    return '';
  }
  if (totals.partial_row_count === 0) {
    return '（全部行已还原干净）';
  }
  return `（其中 ${totals.partial_row_count} 行不彻底，展开看卡在第几层）`;
});

/** 空态文案 */
const detailEmptyText = computed(() => emptyTextOf(rollupData.value, filter));

/** 页脚口径（卷积 + 不彻底 + 后端 note） */
const detailBasis = computed(() => rollupBasis(rollupData.value));

/** 明细表列（一行一个成本对象；宽度按 1400 内容区排得下为主，超了就横向滚） */
const detailColumns: ColumnDef[] = [
  { colKey: 'object', title: '对象号 / 批号', width: 150 },
  { colKey: 'product', title: '产出品 / 车间', width: 170, ellipsis: true },
  { colKey: 'state', title: '状态', width: 96 },
  { colKey: 'before', title: '还原前（料含半成品）', width: 205, ellipsis: true },
  { colKey: 'semi', title: '半成品占材料', width: 112, align: 'right' },
  { colKey: 'after', title: '还原后（料为外购）', width: 205, ellipsis: true },
  { colKey: 'delta', title: '差额（后 − 前）', width: 175, ellipsis: true },
  { colKey: 'unresolved', title: '不彻底', width: 100, align: 'right' },
  { colKey: 'depth', title: '层数', width: 84, align: 'right' },
];

/** 展开行：三要素对照列（窄，跟左边的事实列表并排） */
const elementColumns = [
  { colKey: 'label', title: '要素', width: 72 },
  { colKey: 'before', title: '还原前', width: 96, align: 'right' as const },
  { colKey: 'after', title: '还原后', width: 96, align: 'right' as const },
  { colKey: 'delta', title: '差额', width: 90, align: 'right' as const },
];

/** 展开行：逐层台账列（各层不相加，列头写清） */
const flowColumns = [
  { colKey: 'level', title: '层级', width: 78 },
  { colKey: 'nodes', title: '节点', width: 56, align: 'right' as const },
  { colKey: 'imputed', title: '流入本层', width: 104, align: 'right' as const },
  { colKey: 'purchased', title: '确认外购料', width: 104, align: 'right' as const },
  { colKey: 'labor', title: '确认人工', width: 96, align: 'right' as const },
  { colKey: 'overhead', title: '确认制造费用', width: 110, align: 'right' as const },
  { colKey: 'carried', title: '继续往下拨', width: 104, align: 'right' as const },
  { colKey: 'unresolved', title: '本层不彻底', width: 100, align: 'right' as const },
  { colKey: 'balance', title: '守恒', width: 72, align: 'right' as const },
  { colKey: 'note', title: '口径', width: 240 },
];

/** 展开行：半成品明细列 */
const semiColumns = [
  { colKey: 'material', title: '半成品 / 是否停在这一层', width: 250 },
  { colKey: 'lot', title: '领料批号', width: 120 },
  { colKey: 'imputed', title: '拨下来的钱', width: 104, align: 'right' as const },
  { colKey: 'materialCost', title: '还原后外购料', width: 108, align: 'right' as const },
  { colKey: 'labor', title: '还原后人工', width: 96, align: 'right' as const },
  { colKey: 'overhead', title: '还原后制造费用', width: 116, align: 'right' as const },
  { colKey: 'unresolved', title: '不彻底', width: 88, align: 'right' as const },
  { colKey: 'source', title: '结构来源', width: 96 },
  { colKey: 'producer', title: '产出对象号', width: 160 },
];

const detailFetcher: ListFetcher<RestoreRow> = async (query) => {
  const built = query as RestoreRollupQuery;
  if (!built.period_code) {
    rollupData.value = null;
    return { list: [], total: 0, page: 1, page_size: PAGE_SIZE };
  }
  const res = await fetchRestoreRollup(built);
  rollupData.value = res;
  return res;
};

const detailQueryBuilder: ListQueryBuilder = (_state, page, pageSize): ListQuery =>
  buildRollupQuery({ ...filter }, page, pageSize) ?? {};

function searchDetail(): void {
  void detailListRef.value?.search();
}

// ====================================================================================
// ② 还原后报表
// ====================================================================================

const reportListRef = ref<InstanceType<typeof ListPage> | null>(null);
const reportData = ref<RestoreReportRes | null>(null);

/** 汇总卡片（整表合计：占比 + 结构变化 + 两个自检） */
const reportSummary = computed<SummaryCard[]>(() => reportCards(reportData.value));

/** 页脚口径（分母 + 占比公式 + 结构变化公式） */
const reportBasisText = computed(() => reportBasis(reportData.value));

/** 空态文案 */
const reportEmpty = computed(() => reportEmptyText(reportData.value, filter));

/** 合计旁边的补充说明 */
const reportMetaSuffix = computed(() => {
  const res = reportData.value;
  if (!res || res.total === 0) {
    return '';
  }
  return `（共 ${res.total} 组，一页 ${res.page_size} 组；合计不受分页影响）`;
});

/**
 * 报表列（一行一个分组）。
 *
 * 「占比」列同时显示**还原后占比**与**变化（pp）**：结构变化才是这张报表的重点
 * （材料 −40pp、人工 +13.71pp 这种话在明细表里看不出来）。
 */
const reportColumns: ColumnDef[] = [
  { colKey: 'label', title: '分组', width: 140, ellipsis: true },
  { colKey: 'objects', title: '对象数', width: 100, align: 'right' },
  { colKey: 'before', title: '还原前合计', width: 190, align: 'right', ellipsis: true },
  { colKey: 'after', title: '还原后合计', width: 190, align: 'right', ellipsis: true },
  { colKey: 'materialShare', title: '材料占比 / 变化', width: 116, align: 'right' },
  { colKey: 'laborShare', title: '人工占比 / 变化', width: 116, align: 'right' },
  { colKey: 'overheadShare', title: '费用占比 / 变化', width: 116, align: 'right' },
  { colKey: 'unresolved', title: '不彻底占比', width: 96, align: 'right' },
  { colKey: 'shareSum', title: '占比合计', width: 90, align: 'right' },
  { colKey: 'check', title: '守恒', width: 70, align: 'right' },
];

/**
 * 报表拉一页。
 *
 * 行**直接**用 `groupLines(res)`（展示行与后端分组一一对应，顺序也由后端定：
 * 还原后合计降序 + key 升序），不在这里二次排序 —— 排序口径改了就是两处不一致。
 * `shareRaw` / `checkRaw` 是 tooltip 要显示的原文（展示行里已带）。
 */
const reportFetcher: ListFetcher<GroupLine> = async (query) => {
  const built = query as RestoreReportQuery;
  if (!built.period_code) {
    reportData.value = null;
    return { list: [], total: 0, page: 1, page_size: REPORT_PAGE_SIZE };
  }
  const res = await fetchRestoreReport(built);
  reportData.value = res;
  return { list: groupLines(res), total: res.total, page: res.page, page_size: res.page_size };
};

const reportQueryBuilder: ListQueryBuilder = (_state, page, pageSize): ListQuery =>
  buildReportQuery({ ...filter }, page, pageSize) ?? {};

function searchReport(): void {
  void reportListRef.value?.search();
}
</script>

<style scoped lang="less">
.cr-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.cr-tool-card,
.cr-summary-card {
  border-radius: 8px;
}

.cr-period {
  width: 160px;
}

.cr-state {
  width: 150px;
}

.cr-depth {
  display: flex;
  align-items: center;
  gap: 6px;

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 12px;
    white-space: nowrap;
  }

  :deep(.t-input-number) {
    width: 84px;
  }
}

.cr-group {
  width: 140px;
}

/* ==================== 对象列 ==================== */

.cr-object {
  display: flex;
  flex-direction: column;
  line-height: 1.3;

  &--right {
    align-items: flex-end;
  }

  &__no {
    font-weight: 500;
  }

  &__sub {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

/* ==================== 汇总卡片 ==================== */

.stat-row {
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 12px;
  margin-top: 8px;

  @media (max-width: 1400px) {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}

.stat-card {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 14px 16px;
  border-radius: 8px;
  border: 1px solid var(--td-component-stroke);
  background: var(--td-bg-color-container);

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }

  &__value {
    font-size: 20px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    line-height: 1.2;

    &--primary {
      color: var(--td-brand-color);
    }

    &--bad {
      color: var(--td-error-color);
    }

    &--good {
      color: var(--td-success-color);
    }
  }

  &__sub {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
    line-height: 1.4;
  }
}

.cr-basis {
  margin: 10px 0 0;
  padding: 8px 12px;
  border-left: 3px solid var(--td-brand-color);
  background: var(--td-bg-color-container-hover);
  border-radius: 4px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 20px;
}

/* ==================== 展开行 ==================== */

.cr-detail {
  padding: 4px 8px 8px;

  &__cols {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 16px;
  }

  &__col {
    min-width: 0;
  }

  &__title {
    margin: 10px 0 6px;
    color: var(--td-text-color-secondary);
    font-size: 12px;
    font-weight: 600;
  }
}

.cr-facts {
  display: grid;
  grid-template-columns: max-content minmax(0, 1fr);
  gap: 4px 12px;
  margin: 0;
  font-size: 12px;
  line-height: 20px;

  dt {
    color: var(--td-text-color-placeholder);
    white-space: nowrap;
  }

  dd {
    margin: 0;
    word-break: break-all;
  }
}

.cr-alert {
  margin-top: 10px;
}

/* ==================== 通用文字 ==================== */

.cr-note {
  font-size: 12px;
}

.cr-muted {
  color: var(--td-text-color-placeholder);
}

.cr-strong {
  font-weight: 600;
}

.cr-ok {
  color: var(--td-success-color);
}

.cr-bad {
  color: var(--td-error-color);
}

.mono {
  font-variant-numeric: tabular-nums;
}

.cr-foot-note {
  margin: 8px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 20px;
}
</style>
