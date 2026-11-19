<!--
  在制品页（M5-25【M5·成本】在制品：前端页 · 录入 + 余额）。

  一页两个页签，各是一条完整链路的前端落点：

  ```
  ┌ 页签 ① 完工率录入 = 成本对象列表（一行一批）+ 录入抽屉（逐工序完工率 → 加权综合完工率）
  └ 页签 ② 在制余额   = 期末在制余额表（一行一个成本对象：期初 / 投入 / 完工转出 / 期末 + 结转核对）
  ```

  # 这两页签回答的是同一个问题的两半

  「这笔生产批还有 40 件在制，它值多少钱？这些钱有没有交给下一个月？」

  * **录入**回答前半句的**分母**：车间报「下料完、焊接 60%、装配没开始」⇒ 加权综合完工率 50%
    ⇒ 加工约当 = 完工 + 在制 × 50%（M5-20 约当产量表读的就是这个数）；
  * **余额**回答后半句：期初在制 + 本期投入 − 完工转出 = 期末余额，并逐行核对「本期账面期末
    有没有一分不差地变成下期的期初」（结转八态）。

  # 数据从哪来（两条接口，零新增权限码）

  | 页签 | 接口 | 权限 |
  |---|---|---|
  | 录入 | `GET/PUT /api/business/erp/cost-object/{id}/operations`（M5-21） | `cost-object:list` / `:update` |
  | 余额 | `GET /api/business/erp/cost-wip-balance`（M5-23） | `cost-object:list` |

  两页签都**复用**成本对象的读码（看得到成本对象的人就该看到在制折几件、余额结没结转）；
  写动作走 `cost-object:update`（与「填写本期产量与在制」同一个码，那是同一件事的两半）。

  # 前端不重算任何公式（本页最重要的口径）

  * 综合完工率 = Σ(权重 × 工序完工率) ÷ Σ权重 —— 唯一实现在后端 `rollup_of`；
    **表格改动时页面不预览新汇总**，只提示「下面是上次保存的值」
    （预览会造出「预览 50.00%、保存后 50.01%」这种谁都解释不清的差）；
  * 期初还原、要素拆分、余额倒挤、结转核对全在 `cost_wip_balance_dto`；
    合计直接用后端 `totals`（服务端分页下，拿当前页加出来的合计翻页就变，而财务会拿它跟总账对）。

  # 三种 `null` 不许画成 0（会被读成「没有余额」）

  | 位置 | `null` 的含义 | 页面怎么画 |
  |---|---|---|
  | `rollup` | 该批还没有任何工序 | 「还没有工序」+ 说明对象级完工率沿用哪个值 |
  | `wip_total` 等金额列 | 这一行算不出来（`carry.state = blocked`） | `—` + 展开行逐条列 `issues`（事实 + 怎么修） |
  | `elements[]` | 已结算行的试算与账面不一致，三要素加不齐 | `—` + 一句说明「红冲重算」 |

  # 与「批次成本」页在制余额的区别（别以为是重复页面）

  `/business/erp/cost-batch` 的「在制余额」看的是**归集视角**（一批的累计归集 / 已结转 / 本期投入，
  接口 `cost-collect-sheet/wip-balance`，M5-05）；本页看的是**结转核对视角**
  （期初按上期单位成本还原到料 / 工 / 费、期末差额倒挤、本期账面期末有没有结成下期期初，
  接口 `cost-wip-balance`，M5-23）。两张表的钱对得上，但**回答的问题不一样**：
  前者「钱归集了没有」，后者「钱滚到下个月了没有」。
-->
<template>
  <div class="cw-page">
    <t-tabs v-model="activeTab" class="page-tabs" data-testid="cw-tabs" @change="onTabChange">
      <t-tab-panel value="operations" label="完工率录入" data-testid="cw-tab-operations" />
      <t-tab-panel value="balance" label="在制余额" data-testid="cw-tab-balance" />
    </t-tabs>

    <!-- ==================================================================== -->
    <!-- ① 完工率录入：成本对象列表 → 抽屉里录逐工序完工率                     -->
    <!-- ==================================================================== -->
    <template v-if="activeTab === 'operations'">
      <t-card :bordered="false" class="cw-tool-card">
        <div class="tool-row tool-row--dense">
          <t-input
            v-model="objectFilter.keyword"
            class="tool-row__keyword"
            placeholder="对象号 / 产出批号 / 上游订单号"
            clearable
            data-testid="cw-obj-filter-keyword"
            @enter="searchObjects"
          />
          <t-select
            v-model="objectFilter.status"
            class="cw-status"
            :options="objectStatusOptions"
            placeholder="全部状态"
            data-testid="cw-obj-filter-status"
            @change="searchObjects"
          />
          <t-button theme="primary" data-testid="cw-obj-btn-search" @click="searchObjects">查询</t-button>
          <t-button variant="outline" data-testid="cw-obj-btn-reset" @click="resetObjects">重置</t-button>
          <span class="tool-row__spacer" />
          <span class="tool-row__hint">
            点「录入完工率」按工序填进度；保存后自动回写对象级完工率（约当产量表读它）
          </span>
        </div>
      </t-card>

      <ListPage
        ref="objectListRef"
        title="成本对象"
        meta-unit="批"
        testid="cw-obj"
        table-testid="cw-obj-table"
        meta-testid="cw-obj-meta"
        btn-prefix="cw-obj-btn"
        row-key="id"
        :columns="objectColumns"
        :filters="[]"
        :filter-state="objectFilter"
        :fetcher="objectFetcher"
        :query-builder="objectQueryBuilder"
        :actions="objectActions"
        :exportable="false"
        :page-size="PAGE_SIZE"
      >
        <template #operation_no="{ row }">
          <div class="cw-object">
            <span class="cw-object__no">{{ row.object_no }}</span>
            <span class="cw-object__sub">{{ row.lot_no ? `批号 ${row.lot_no}` : '（无产出批号）' }}</span>
          </div>
        </template>

        <template #product="{ row }">
          <div class="cw-object">
            <span>{{ row.product_name || '—' }}</span>
            <span class="cw-object__sub">{{ row.source_doc_no ? `订单 ${row.source_doc_no}` : '（无上游订单）' }}</span>
          </div>
        </template>

        <template #status="{ row }">
          <t-tag size="small" variant="light" :theme="objectStatusTheme(row.status)">
            {{ objectStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #empty>
          <StateView state="empty" compact :title="objectEmptyText(objectFilter)" testid="cw-obj-empty" />
        </template>
      </ListPage>

      <!-- ==================== 录入抽屉 ==================== -->
      <t-drawer
        v-model:visible="editorVisible"
        size="1040px"
        :header="editorHeader"
        :footer="false"
        data-testid="cw-editor-drawer"
      >
        <t-loading :loading="editorLoading" size="small">
          <template v-if="editorRes">
            <!-- ① 抬头 -->
            <t-descriptions :column="3" size="small" bordered data-testid="cw-editor-head">
              <t-descriptions-item label="对象号">{{ editorRes.object.object_no }}</t-descriptions-item>
              <t-descriptions-item label="状态">{{ objectStatusLabel(editorRes.object.status) }}</t-descriptions-item>
              <t-descriptions-item label="投料方式">
                {{ editorRes.object.material_input_mode === 'AS_PROGRESS' ? '随进度投料' : '一次投料' }}
              </t-descriptions-item>
              <t-descriptions-item label="产出品" :span="2">
                {{ editorRes.object.product_code ? `${editorRes.object.product_code} ` : ''
                }}{{ editorRes.object.product_name || '—' }}
              </t-descriptions-item>
              <t-descriptions-item label="生产车间">
                {{ editorRes.object.workshop_dept_name || '—' }}
              </t-descriptions-item>
              <t-descriptions-item label="计划数量">{{ qtyText(editorRes.object.plan_qty) }}</t-descriptions-item>
              <t-descriptions-item label="累计完工">{{ qtyText(editorRes.object.completed_qty) }}</t-descriptions-item>
              <t-descriptions-item label="期末在制">{{ qtyText(editorRes.object.wip_qty) }}</t-descriptions-item>
              <t-descriptions-item label="对象级完工率">
                <span class="mono cw-strong" data-testid="cw-editor-object-pct">
                  {{ pctText(editorRes.object.completion_pct) }}
                </span>
              </t-descriptions-item>
            </t-descriptions>

            <!-- ② 锁：业务状态不允许（不是没权限），原因整段展示 -->
            <t-alert
              v-if="!canSave(editorRes)"
              theme="warning"
              class="cw-alert"
              :message="lockedHint(editorRes)"
              data-testid="cw-editor-locked"
            />

            <!-- ③ 汇总（后端算的，前端不预览未保存的改动） -->
            <div class="cw-rollup" data-testid="cw-editor-rollup">
              <div class="cw-rollup__main">
                <span class="cw-rollup__label">当前已保存的综合完工率</span>
                <span class="cw-rollup__value mono" data-testid="cw-editor-rollup-pct">
                  {{ editorRes.rollup ? pctText(editorRes.rollup.pct) : '—' }}
                </span>
                <span class="cw-rollup__meta" data-testid="cw-editor-rollup-text">{{ rollupText(editorRes) }}</span>
              </div>
              <div v-if="editorRes.rollup" class="cw-rollup__facts">
                <span>
                  Σ(权重 × 完工率) = <span class="mono">{{ qtyText(editorRes.rollup.weighted_sum) }}</span>
                </span>
                <span>
                  未收口商 = <span class="mono">{{ exactText(editorRes.rollup.pct_unrounded) }}</span>
                </span>
                <span :class="rollupMatched(editorRes.rollup) ? 'cw-ok' : 'cw-bad'">
                  对象级 {{
                    rollupMatched(editorRes.rollup) ? '与汇总一致' : `与汇总不一致（差 ${exactText(editorRes.rollup.diff)}）`
                  }}
                </span>
              </div>
            </div>

            <t-alert
              v-if="dirty"
              theme="info"
              class="cw-alert"
              :message="noteText(DIRTY_ROLLUP_HINT)"
              data-testid="cw-editor-dirty"
            />

            <!-- ④ 工序表（可编辑；整表覆盖） -->
            <div class="panel-head">
              <div class="panel-head__main">
                <span class="panel-head__title">工序完工率</span>
                <span class="panel-head__meta">
                  保存 = <b>整表覆盖</b>：删掉一行就少一道工序（没有单独删除接口）
                </span>
              </div>
              <t-button
                size="small"
                variant="outline"
                :disabled="!canSave(editorRes) || editorRows.length >= MAX_ROWS"
                data-testid="cw-editor-add-row"
                @click="addRow"
              >
                添加工序
              </t-button>
            </div>

            <t-table
              row-key="key"
              size="small"
              :data="editorRows"
              :columns="editorColumns"
              :bordered="true"
              data-testid="cw-editor-table"
            >
              <template #seq="{ row }">
                <t-input
                  v-model="row.seq"
                  size="small"
                  :status="editorErrors[row.key]?.seq ? 'error' : undefined"
                  :tips="editorErrors[row.key]?.seq"
                  placeholder="1–999"
                  data-testid="cw-editor-seq"
                />
              </template>

              <template #operation_name="{ row }">
                <t-input
                  v-model="row.operation_name"
                  size="small"
                  placeholder="如：下料 / 焊接 / 装配"
                  :status="editorErrors[row.key]?.operation_name ? 'error' : undefined"
                  :tips="editorErrors[row.key]?.operation_name"
                  data-testid="cw-editor-name"
                />
              </template>

              <template #weight="{ row }">
                <t-input
                  v-model="row.weight"
                  size="small"
                  placeholder="缺省 1"
                  :status="editorErrors[row.key]?.weight ? 'error' : undefined"
                  :tips="editorErrors[row.key]?.weight"
                  data-testid="cw-editor-weight"
                />
              </template>

              <template #completion_pct="{ row }">
                <t-input
                  v-model="row.completion_pct"
                  size="small"
                  placeholder="0–100"
                  :status="editorErrors[row.key]?.completion_pct ? 'error' : undefined"
                  :tips="editorErrors[row.key]?.completion_pct"
                  data-testid="cw-editor-pct"
                />
              </template>

              <!-- 权重占比 / 贡献：**后端算的**，表格一改就过时 ⇒ 脏了画「—」 -->
              <template #share="{ row }">
                <span class="mono cw-muted">{{ shareOf(row) }}</span>
              </template>

              <template #remark="{ row }">
                <t-input
                  v-model="row.remark"
                  size="small"
                  placeholder="备注（可空）"
                  :status="editorErrors[row.key]?.remark ? 'error' : undefined"
                  :tips="editorErrors[row.key]?.remark"
                  data-testid="cw-editor-remark"
                />
              </template>

              <template #op="{ row }">
                <t-link
                  theme="danger"
                  :disabled="!canSave(editorRes)"
                  data-testid="cw-editor-remove"
                  @click="dropRow(row.key)"
                >
                  删除
                </t-link>
              </template>
            </t-table>

            <p class="cw-foot-note" data-testid="cw-editor-formula">
              {{ noteText(editorRes.formula) }}
            </p>
            <p class="cw-foot-note">{{ noteText(editorRes.weight_hint) }}</p>

            <!-- ⑤ 后端提醒（只提示不拦） -->
            <t-alert
              v-for="(warn, index) in editorRes.warnings"
              :key="`warn-${index}`"
              theme="warning"
              class="cw-alert"
              :message="warn"
              data-testid="cw-editor-warning"
            />

            <!-- ⑥ 保存条 -->
            <div class="cw-save-bar">
              <t-button
                v-privilege="[PERM.write]"
                theme="primary"
                :loading="saving"
                :disabled="!canSave(editorRes) || !validation.ok"
                data-testid="cw-editor-save"
                @click="submitEditor"
              >
                保存工序完工率
              </t-button>
              <span v-if="savedMsg" class="cw-ok" data-testid="cw-editor-saved">{{ savedMsg }}</span>
              <span v-if="!validation.ok" class="cw-bad" data-testid="cw-editor-invalid">
                还有 {{ Object.keys(validation.rows).length }} 行填写有误，先按提示改
              </span>
              <span class="cw-muted">{{ noteText(editorRes.note) }}</span>
            </div>

            <t-alert
              v-if="editorError"
              theme="error"
              class="cw-alert"
              :message="editorError"
              data-testid="cw-editor-error"
            />
          </template>
        </t-loading>
      </t-drawer>
    </template>

    <!-- ==================================================================== -->
    <!-- ② 在制余额：期初 + 投入 − 完工转出 = 期末（含结转核对）                -->
    <!-- ==================================================================== -->
    <template v-else>
      <t-card :bordered="false" class="cw-tool-card">
        <div class="tool-row tool-row--dense">
          <t-date-picker
            v-model="balanceFilter.period_code"
            class="cw-period"
            mode="month"
            value-type="YYYY-MM"
            format="YYYY-MM"
            placeholder="会计期间（必选）"
            data-testid="cw-balance-filter-period"
            @change="searchBalance"
          />
          <t-input
            v-model="balanceFilter.keyword"
            class="tool-row__keyword"
            placeholder="对象号 / 批号 / 产出品编码 / 名称"
            clearable
            data-testid="cw-balance-filter-keyword"
            @enter="searchBalance"
          />
          <t-select
            v-model="balanceFilter.state"
            class="cw-state"
            :options="balanceStateOptions"
            placeholder="有余额"
            data-testid="cw-balance-filter-state"
            @change="searchBalance"
          />
          <t-button theme="primary" :disabled="!periodReady" data-testid="cw-balance-btn-search" @click="searchBalance">
            查询
          </t-button>
          <t-button variant="outline" data-testid="cw-balance-btn-reset" @click="resetBalance">重置</t-button>
          <span class="tool-row__spacer" />
          <span class="tool-row__hint">期末余额 = 期初 + 投入 − 完工转出（差额倒挤；期间必填）</span>
        </div>
      </t-card>

      <t-alert
        v-if="!periodReady"
        theme="info"
        :message="PERIOD_REQUIRED_HINT"
        data-testid="cw-balance-period-hint"
      />

      <t-alert v-else-if="scopeHint" theme="warning" :message="scopeHint" data-testid="cw-balance-scope-hint">
        <template #operation>
          <t-button size="small" variant="outline" data-testid="cw-balance-btn-to-cost-period" @click="goCostPeriod">
            去成本核算
          </t-button>
        </template>
      </t-alert>

      <!-- 整表合计（后端按筛选后全集算的，翻页不变） -->
      <t-card v-if="balanceData && balanceData.total > 0" :bordered="false" class="cw-summary-card">
        <div class="panel-head">
          <div class="panel-head__main">
            <span class="panel-head__title">整表合计</span>
            <span class="panel-head__meta" data-testid="cw-balance-scope-meta">
              {{ balanceData.period_code }}
              <template v-if="balanceData.batch"> · 批次 {{ balanceData.batch.batch_no }}（{{ balanceData.batch.status }}）</template>
              <template v-else> · 无成本核算批次</template>
            </span>
          </div>
        </div>

        <div class="stat-row" data-testid="cw-balance-stats">
          <div v-for="card in summary" :key="card.key" class="stat-card" :data-testid="`cw-balance-stat-${card.key}`">
            <span class="stat-card__label">{{ card.label }}</span>
            <span class="stat-card__value" :class="summaryValueClass(card.tone)">{{ card.value }}</span>
            <span class="stat-card__sub">{{ card.sub }}</span>
          </div>
        </div>

        <p class="cw-basis" data-testid="cw-balance-basis">
          {{ noteText(balanceData.formula_roll) }}
        </p>
        <p class="cw-basis" data-testid="cw-balance-carry-basis">
          {{ noteText(balanceData.formula_carry) }}
        </p>
      </t-card>

      <ListPage
        ref="balanceListRef"
        class="cw-list"
        title="在制余额"
        meta-unit="批"
        testid="cw-balance"
        table-testid="cw-balance-table"
        meta-testid="cw-balance-meta"
        btn-prefix="cw-balance-btn"
        row-key="object_id"
        expandable
        :table-slots="['expandedRow']"
        :exportable="false"
        :columns="balanceColumns"
        :filters="[]"
        :fetcher="balanceFetcher"
        :query-builder="balanceQueryBuilder"
        :page-size="PAGE_SIZE"
      >
        <template #meta-extra>
          <span class="cw-muted">{{ balanceMetaSuffix }}</span>
        </template>

        <template #object="{ row }">
          <div class="cw-object">
            <span class="cw-object__no">{{ row.object_no }}</span>
            <span class="cw-object__sub">{{ row.lot_no ? `批号 ${row.lot_no}` : '（无产出批号）' }}</span>
          </div>
        </template>

        <template #product="{ row }">
          <div class="cw-object">
            <span>{{ productText(row) }}</span>
            <span class="cw-object__sub">{{ row.workshop_dept_name || '（无车间）' }}</span>
          </div>
        </template>

        <template #qty="{ row }">
          <div class="cw-object cw-object--right">
            <span class="mono">{{ qtyText(row.wip_qty) }}</span>
            <span class="cw-object__sub">
              期初 {{ qtyText(row.beg_qty) }} · 本期完工 {{ qtyText(row.completed_qty) }}
            </span>
          </div>
        </template>

        <template #pct="{ row }">
          <span class="mono">{{ pctText(row.completion_pct) }}</span>
        </template>

        <template #beg="{ row }">
          <span class="mono cw-muted">{{ amountOrDash(row.beg_total) }}</span>
        </template>

        <template #cur="{ row }">
          <span class="mono">{{ amountOrDash(row.cur_total) }}</span>
        </template>

        <template #finished="{ row }">
          <span class="mono">{{ amountOrDash(row.finished_total) }}</span>
        </template>

        <!-- 期末余额：算不出来画「—」并说明，不画 0.00 -->
        <template #wip="{ row }">
          <span v-if="row.wip_total === null" class="cw-muted">—</span>
          <span v-else class="mono cw-strong" :class="toneClass(wipTone(row))">{{ moneyText(row.wip_total) }}</span>
        </template>

        <template #elements="{ row }">
          <span class="mono cw-elements-text">{{ elementsCompactText(row) }}</span>
        </template>

        <template #carry="{ row }">
          <t-tooltip v-if="row.carry?.hint" :content="row.carry.hint" placement="top-left">
            <t-tag size="small" variant="light" :theme="carryTheme(row.carry.state)">
              {{ carryText(row.carry) }}
            </t-tag>
          </t-tooltip>
          <t-tag v-else size="small" variant="light" :theme="carryTheme(row.carry.state)">
            {{ carryText(row.carry) }}
          </t-tag>
        </template>

        <template #source="{ row }">
          <span class="cw-muted">{{ row.source_label || row.source }}</span>
        </template>

        <template #empty>
          <StateView
            state="empty"
            compact
            :title="emptyText"
            description="换一个期间 / 关键字 / 状态，或先到「成本核算」把该期间算出来。"
            testid="cw-balance-empty"
          />
        </template>

        <!-- 展开行：数量口径 + 三要素 + 账面与结转 + 怎么修 -->
        <template #expandedRow="{ row }">
          <div class="cw-detail" data-testid="cw-balance-detail">
            <div class="cw-detail__cols">
              <div class="cw-detail__col">
                <div class="cw-detail__title">数量口径</div>
                <dl class="cw-facts">
                  <template v-for="fact in quantityFacts(row)" :key="`q-${fact.label}`">
                    <dt>{{ fact.label }}</dt>
                    <dd class="mono">{{ fact.value }}</dd>
                  </template>
                </dl>
              </div>
              <div class="cw-detail__col">
                <div class="cw-detail__title">账面与结转</div>
                <dl class="cw-facts" data-testid="cw-balance-detail-ledger">
                  <template v-for="fact in ledgerFacts(row)" :key="`l-${fact.label}`">
                    <dt>{{ fact.label }}</dt>
                    <dd class="mono" :class="toneClass(fact.tone ?? 'default')">{{ fact.value }}</dd>
                  </template>
                </dl>
              </div>
            </div>

            <div class="cw-detail__title">三要素滚动（料 / 工 / 费）</div>
            <t-alert
              v-if="!row.elements_available"
              theme="info"
              class="cw-alert"
              :message="ELEMENTS_UNAVAILABLE_HINT"
              data-testid="cw-balance-detail-elements-hint"
            />
            <t-table
              row-key="key"
              size="small"
              :data="elementLines(row.elements)"
              :columns="elementColumns"
              data-testid="cw-balance-detail-elements"
            >
              <template #label="{ row: element }">
                <span class="cw-strong">{{ element.label }}</span>
              </template>
              <template #beg="{ row: element }">
                <span class="mono cw-muted">{{ element.beg }}</span>
              </template>
              <template #cur="{ row: element }">
                <span class="mono">{{ element.cur }}</span>
              </template>
              <template #finished="{ row: element }">
                <span class="mono">{{ element.finished }}</span>
              </template>
              <template #wip="{ row: element }">
                <span class="mono cw-strong">{{ element.wip }}</span>
              </template>
            </t-table>

            <template v-if="rowHints(row).length > 0">
              <div class="cw-detail__title">这一行怎么理解 / 怎么修</div>
              <t-alert
                v-for="(hint, index) in rowHints(row)"
                :key="`h-${index}`"
                :theme="needsAttention(row.carry) ? 'warning' : 'info'"
                class="cw-alert"
                :message="hint"
                data-testid="cw-balance-detail-hint"
              />
            </template>
          </div>
        </template>

        <template #footer>
          <p v-if="balanceNote" class="cw-foot-note" data-testid="cw-balance-note">{{ balanceNote }}</p>
        </template>
      </ListPage>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';

import {
  fetchCostObjectPage,
  type CostObjectRow,
} from '@/api/business/erp/cost-object';
import {
  COST_OBJECT_STATUS_OPTIONS,
  MAX_ROWS,
  PAGE_SIZE,
  fetchWipBalance,
  fetchWipOperations,
  saveWipOperations,
  type WipBalanceQuery,
  type WipBalanceRes,
  type WipBalanceRow,
  type WipOperationRes,
} from '@/api/business/erp/cost-wip';
import {
  BALANCE_STATE_FILTERS,
  COST_WIP_PERMISSIONS,
} from '@/api/business/erp/cost-wip-kinds';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, ListActions, ListFetcher, ListQuery, ListQueryBuilder } from '@/components/ListPage/types';
import StateView from '@/components/StateView/index.vue';
import {
  DIRTY_ROLLUP_HINT,
  ELEMENTS_UNAVAILABLE_HINT,
  PERIOD_REQUIRED_HINT,
  amountOrDash,
  appendRow,
  buildBalanceQuery,
  buildObjectQuery,
  canSave,
  carryText,
  carryTheme,
  contributionText,
  defaultBalanceFilter,
  defaultObjectFilter,
  editorRowsFromRes,
  elementLines,
  emptyTextOf,
  exactText,
  ledgerFacts,
  lockedHint,
  moneyText,
  needsAttention,
  noteText,
  objectEmptyText,
  pctText,
  productText,
  qtyText,
  quantityFacts,
  removeRow,
  rollupMatched,
  rollupText,
  rowHints,
  saveErrorText,
  savedText,
  scopeHintOf,
  shareText,
  summaryCards,
  toSavePayload,
  validateRows,
  wipTone,
  type BalanceFilter,
  type EditorRow,
  type EditorRowErrors,
  type ObjectFilter,
} from './cost-wip-logic';

const PERM = COST_WIP_PERMISSIONS;

const router = useRouter();

/** 一个页签的切换（`t-tabs` 的 `change` 给的是 value） */
const activeTab = ref<'operations' | 'balance'>('operations');

// ==================== 共用小件 ====================

const objectStatusOptions = COST_OBJECT_STATUS_OPTIONS.map((option) => ({
  label: option.label,
  value: option.value,
}));

const balanceStateOptions = BALANCE_STATE_FILTERS.map((option) => ({
  label: option.label,
  value: option.value,
}));

/** 成本对象状态中文（`OPEN` / `CLOSED` / `CANCELLED`） */
function objectStatusLabel(status: string | null | undefined): string {
  return COST_OBJECT_STATUS_OPTIONS.find((option) => option.value === status)?.label ?? (status ?? '—');
}

/** 成本对象状态 → tag 主题 */
function objectStatusTheme(status: string | null | undefined): 'success' | 'default' | 'danger' {
  if (status === 'OPEN') {
    return 'success';
  }
  if (status === 'CANCELLED') {
    return 'danger';
  }
  return 'default';
}

/** 语义色 class（红 / 绿 / 无色） */
function toneClass(tone: 'default' | 'danger' | 'success'): string {
  if (tone === 'danger') {
    return 'cw-bad';
  }
  if (tone === 'success') {
    return 'cw-ok';
  }
  return '';
}

/** 汇总卡片主值 class */
function summaryValueClass(tone: string): string {
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

function onTabChange(value: string | number): void {
  if (value === 'balance') {
    searchBalance();
  } else {
    searchObjects();
  }
}

// ====================================================================================
// ① 完工率录入
// ====================================================================================

const objectFilter = reactive<ObjectFilter>(defaultObjectFilter());
const objectListRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 列表列（对象号 / 产出品 / 数量 / 状态） */
const objectColumns: ColumnDef[] = [
  { colKey: 'operation_no', title: '对象号', width: 190 },
  { colKey: 'product', title: '产出品 / 上游订单', width: 220, ellipsis: true },
  { colKey: 'plan_qty', title: '计划数量', width: 100, align: 'right' },
  { colKey: 'completed_qty', title: '累计完工', width: 100, align: 'right' },
  { colKey: 'wip_qty', title: '期末在制', width: 100, align: 'right' },
  { colKey: 'status', title: '状态', width: 90 },
];

/** 行内操作：打开录入抽屉（**不按状态置灰**：已关批的批也要能看工序，抽屉里会说为什么不能改） */
const objectActions: ListActions<CostObjectRow> = {
  row: [
    {
      key: 'operations',
      label: '录入完工率',
      permission: PERM.read,
      testid: 'cw-obj-row-operations',
      handler: (row) => {
        void openEditor(row);
      },
    },
  ],
};

const objectFetcher: ListFetcher<CostObjectRow> = (query) =>
  fetchCostObjectPage(query as Parameters<typeof fetchCostObjectPage>[0]);

const objectQueryBuilder: ListQueryBuilder = (_state, page, pageSize): ListQuery =>
  buildObjectQuery({ ...objectFilter }, page, pageSize) as ListQuery;

function searchObjects(): void {
  void objectListRef.value?.search();
}

function resetObjects(): void {
  Object.assign(objectFilter, defaultObjectFilter());
  searchObjects();
}

// ---------- 录入抽屉 ----------

const editorVisible = ref(false);
const editorLoading = ref(false);
const saving = ref(false);
const editorRes = ref<WipOperationRes | null>(null);
const editorRows = ref<EditorRow[]>([]);
const editorError = ref('');
const savedMsg = ref('');
/** 是否已经点过保存（点过一次才开始在格子上标红：输入过程中不要实时骂人） */
const showErrors = ref(false);
/** 上次保存 / 加载时的 payload 快照（判「有没有改动」） */
const savedSnapshot = ref('');
const editingObjectId = ref('');

/**
 * 工序表列（**可编辑**）。
 *
 * 「权重占比 / 贡献（已存）」这一列是**后端算的**（`share_pct` / `contribution_pct`），
 * 表格一改就过时 ⇒ `shareOf()` 在脏态画「—」。
 */
const editorColumns = [
  { colKey: 'seq', title: '序号', width: 84 },
  { colKey: 'operation_name', title: '工序名', width: 190 },
  { colKey: 'weight', title: '权重', width: 96 },
  { colKey: 'completion_pct', title: '本工序完工率 %', width: 130 },
  { colKey: 'share', title: '权重占比 / 贡献（已存）', width: 170 },
  { colKey: 'remark', title: '备注', width: 190 },
  { colKey: 'op', title: '操作', width: 70 },
];

const editorHeader = computed(() => {
  const no = editorRes.value?.object.object_no ?? '';
  return no ? `${no} · 工序完工率` : '工序完工率';
});

/** 当前 payload 的序列化快照（用于判脏） */
const currentSnapshot = computed(() => JSON.stringify(toSavePayload(editorRows.value)));

/** 表格是否有未保存的改动 */
const dirty = computed(() => editorRes.value !== null && currentSnapshot.value !== savedSnapshot.value);

/** 前置校验（值域与后端 `specs_of` 一致；真正的权威仍是后端 400） */
const validation = computed(() => validateRows(editorRows.value));

/**
 * 行 key → 字段错误。
 *
 * 只有点过「保存」才标红（`showErrors`）：边打字边报错会让人烦，
 * 但保存前一次性把「哪一行哪一格不对」全标出来．
 */
const editorErrors = computed<Record<string, EditorRowErrors>>(() => {
  const map: Record<string, EditorRowErrors> = {};
  if (!showErrors.value) {
    return map;
  }
  editorRows.value.forEach((row, index) => {
    const err = validation.value.rows[index];
    if (err) {
      map[row.key] = err;
    }
  });
  return map;
});

async function openEditor(row: CostObjectRow): Promise<void> {
  editorVisible.value = true;
  editorLoading.value = true;
  editorError.value = '';
  savedMsg.value = '';
  showErrors.value = false;
  editingObjectId.value = row.id;
  try {
    const res = await fetchWipOperations(row.id);
    editorRes.value = res;
    editorRows.value = editorRowsFromRes(res);
    savedSnapshot.value = JSON.stringify(toSavePayload(editorRows.value));
  } catch (error) {
    editorRes.value = null;
    editorRows.value = [];
    editorError.value = saveErrorText(error);
  } finally {
    editorLoading.value = false;
  }
}

function addRow(): void {
  appendRow(editorRows.value);
  savedMsg.value = '';
}

function dropRow(key: string): void {
  removeRow(editorRows.value, key);
  savedMsg.value = '';
}

/**
 * 行内「权重占比 / 贡献」。
 *
 * 两个数都是**后端**按已保存的行算的（`share_pct` / `contribution_pct`），
 * 表格一改它们就过时 —— 所以脏了画「—」并提示「保存后重算」，
 * 绝不用界面上的值现算（那又变成两份公式）。
 */
function shareOf(row: EditorRow): string {
  if (dirty.value) {
    return '—';
  }
  const saved = editorRes.value?.rows.find((item) => String(item.seq) === row.seq.trim());
  if (!saved) {
    return '—';
  }
  return `${shareText(saved)} / ${contributionText(saved)}`;
}

async function submitEditor(): Promise<void> {
  if (!editorRes.value || saving.value) {
    return;
  }
  showErrors.value = true;
  if (!validation.value.ok) {
    editorError.value = validation.value.messages.join('；');
    return;
  }
  saving.value = true;
  editorError.value = '';
  savedMsg.value = '';
  try {
    const res = await saveWipOperations(editingObjectId.value, toSavePayload(editorRows.value));
    editorRes.value = res;
    editorRows.value = editorRowsFromRes(res);
    savedSnapshot.value = JSON.stringify(toSavePayload(editorRows.value));
    showErrors.value = false;
    savedMsg.value = savedText(res);
    // 列表里的「累计完工 / 在制」没变，但对象级完工率可能变了 —— 顺手刷新当前页
    objectListRef.value?.reload();
  } catch (error) {
    editorError.value = saveErrorText(error);
  } finally {
    saving.value = false;
  }
}

// ====================================================================================
// ② 在制余额
// ====================================================================================

const balanceFilter = reactive<BalanceFilter>(defaultBalanceFilter());
const balanceListRef = ref<InstanceType<typeof ListPage> | null>(null);
const balanceData = ref<WipBalanceRes | null>(null);

const periodReady = computed(() => buildBalanceQuery({ ...balanceFilter }, 1) !== null);
const summary = computed(() => summaryCards(balanceData.value));
const scopeHint = computed(() => scopeHintOf(balanceData.value));
const balanceNote = computed(() => noteText(balanceData.value?.note));
const emptyText = computed(() => emptyTextOf(balanceData.value, { ...balanceFilter }));

/** 「共 N 行」后缀（把需要处理的行数说在同一个元素里） */
const balanceMetaSuffix = computed(() => {
  const totals = balanceData.value?.totals;
  if (!totals || totals.row_count === 0) {
    return '';
  }
  const attention = totals.mismatch_count + totals.unsettled_count + totals.reversed_count + totals.closed_wip_count + totals.blocked_count;
  return attention > 0 ? `（其中 ${attention} 行需要处理，见状态列）` : '';
});

const balanceFetcher: ListFetcher<WipBalanceRow> = async (query) => {
  const built = query as WipBalanceQuery;
  if (!built.period_code) {
    balanceData.value = null;
    return { list: [], total: 0, page: 1, page_size: PAGE_SIZE };
  }
  const res = await fetchWipBalance(built);
  balanceData.value = res;
  return res;
};

const balanceQueryBuilder: ListQueryBuilder = (_state, page, pageSize): ListQuery =>
  buildBalanceQuery({ ...balanceFilter }, page, pageSize) ?? {};

function searchBalance(): void {
  void balanceListRef.value?.search();
}

function resetBalance(): void {
  Object.assign(balanceFilter, defaultBalanceFilter());
  searchBalance();
}

function goCostPeriod(): void {
  void router.push('/business/erp/cost-period');
}

/** 余额表列（一行一个成本对象） */
const balanceColumns: ColumnDef[] = [
  { colKey: 'object', title: '对象号 / 批号', width: 180 },
  { colKey: 'product', title: '产出品 / 车间', width: 210, ellipsis: true },
  { colKey: 'qty', title: '期末在制', width: 150, align: 'right' },
  { colKey: 'pct', title: '完工率', width: 84, align: 'right' },
  { colKey: 'beg', title: '期初在制', width: 118, align: 'right' },
  { colKey: 'cur', title: '本期投入', width: 118, align: 'right' },
  { colKey: 'finished', title: '完工转出', width: 118, align: 'right' },
  { colKey: 'wip', title: '期末余额', width: 128, align: 'right' },
  { colKey: 'elements', title: '料 / 工 / 费', width: 190, ellipsis: true },
  { colKey: 'carry', title: '结转', width: 96 },
  { colKey: 'source', title: '口径', width: 100 },
];

/** 展开行里的三要素表列 */
const elementColumns = [
  { colKey: 'label', title: '要素', width: 110 },
  { colKey: 'beg', title: '期初在制', width: 130, align: 'right' as const },
  { colKey: 'cur', title: '本期投入', width: 130, align: 'right' as const },
  { colKey: 'finished', title: '完工转出', width: 130, align: 'right' as const },
  { colKey: 'wip', title: '期末余额', width: 130, align: 'right' as const },
];

/** 「料 100.00 · 工 50.00 · 费 20.00」（分要素不可用时说明原因） */
function elementsCompactText(row: WipBalanceRow): string {
  if (!row.elements_available) {
    return '—（分要素不可用）';
  }
  const lines = elementLines(row.elements);
  return lines.map((line) => `${line.label.slice(0, 1)} ${line.wip}`).join(' · ');
}
</script>

<style scoped lang="less">
.cw-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.cw-tool-card,
.cw-summary-card {
  border-radius: 8px;
}

.cw-status {
  width: 140px;
}

.cw-period {
  width: 160px;
}

.cw-state {
  width: 150px;
}

/* ==================== 对象列 ==================== */

.cw-object {
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

.cw-basis {
  margin: 10px 0 0;
  padding: 8px 12px;
  border-left: 3px solid var(--td-brand-color);
  background: var(--td-bg-color-container-hover);
  border-radius: 4px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 20px;
}

/* ==================== 录入抽屉 ==================== */

.cw-alert {
  margin-top: 10px;
}

.cw-rollup {
  margin-top: 12px;
  padding: 12px 14px;
  border-radius: 8px;
  border: 1px solid var(--td-component-stroke);
  background: var(--td-bg-color-container-hover);

  &__main {
    display: flex;
    align-items: baseline;
    gap: 12px;
    flex-wrap: wrap;
  }

  &__label {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }

  &__value {
    font-size: 22px;
    font-weight: 600;
    color: var(--td-brand-color);
  }

  &__meta {
    color: var(--td-text-color-secondary);
    font-size: 12px;
  }

  &__facts {
    display: flex;
    gap: 16px;
    flex-wrap: wrap;
    margin-top: 6px;
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

.cw-save-bar {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
  margin-top: 12px;
  padding-top: 12px;
  border-top: 1px solid var(--td-component-stroke);
  font-size: 12px;
}

/* ==================== 余额展开行 ==================== */

.cw-detail {
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
    margin: 8px 0 6px;
    color: var(--td-text-color-secondary);
    font-size: 12px;
    font-weight: 600;
  }
}

.cw-facts {
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

/* ==================== 通用文字 ==================== */

.cw-elements-text {
  font-size: 12px;
}

.cw-muted {
  color: var(--td-text-color-placeholder);
}

.cw-strong {
  font-weight: 600;
}

.cw-ok {
  color: var(--td-success-color);
}

.cw-bad {
  color: var(--td-error-color);
}

.mono {
  font-variant-numeric: tabular-nums;
}

.cw-foot-note {
  margin: 8px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 20px;
}
</style>
