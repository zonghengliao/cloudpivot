<!--
  批次成本页（M5-07 · 分批法「批次成本明细」· ARCH-9 §6.1 / M5-02…M5-06 的前端落点）。

  一页两个视图 + 一个批次抽屉：

  ```
  ┌ 页签 ① 批次成本 = 归集表（一行一批：料 / 工 / 费 / 合计 + 数量进度 + 表尾整表合计）
  └ 页签 ② 在制余额 = 一批一行（累计归集 / 已结转 / 在制余额 / 本期投入 / 本期转出）
        ↓ 点批次号 / 行
     抽屉（4 个子页签）
       ∟ 归集明细：本期逐笔归集行（料从哪张领料单来、工是哪几张工时单、费是哪几笔台账）
       ∟ 来源与归集：三路候选（已归集 / 待归集 / 不可归集）+ 「归集本期」按钮
       ∟ 完工结转：闸门（为什么不能转 / 怎么修）+ 本次金额与单位成本 + 历史 + 「结转」按钮
       ∟ 在制台账：逐期「期初 / 投入 / 转出 / 期末」+ 警告 + 口径说明
  ```

  # 数据全部来自后端，页面不自己算账

  8 条接口（见 `api/business/erp/cost-batch.ts`）。**页面唯一的算术是「三项相加」**
  （`SheetAmounts.mat+labor+oh` 与 `WipAmounts.material+labor+overhead`），
  且走整数分（`shared/money::toCents`）—— 归集表行、明细合计、余额表行、台账期末
  四处相加必须与后端一致，浮点相加会在第三位小数上假报警。

  # 四条口径（后端逐字同源，这页不许弱化）

  1. **期间必填**：`period_code` 没有默认值（`cost_collect_sheet_dto::period_of` 明写
     「归集额是期间 × 批次的量」）。页面用 `todayPeriod()` 做初值 + **记住上次选择**
     （localStorage），因为每月开局第一件事就是切期间；
  2. **不能归集 / 不能结转时，先说「为什么」再说「怎么修」**：三路来源的
     `block_reason + block_fix`、结转的 `blockers[].message + fix` 一律整段展示。
     只显示「不行」不显示「去哪补单」，用户只能去问会计；
  3. **归集 / 结转按钮置灰的原因写在按钮旁边**（`finishDisabledReason`），不是点下去才吃 409；
  4. **在制余额不夹零**：负数是异常（转出多于归集 = 来源被作废 / 有人改库），
     页面照原样显示并挂红字，不替后端「修正」。

  # 没有的东西（别以为是漏了）

  * **没有「手工填金额」的落脚点**：`POST …/collect` 的入参只有 `period_code`，
    金额由领料单 / 工时台账 / 费用台账推导（`cost_batch_collect_dto` 模块头）；
  * **没有删除 / 作废批次**：作废走成本对象页（`cost-object`），本页只读 + 归集 + 结转；
  * **没有导出**：工资表式的 xlsx 导出属成本对象 / 核算页，本页不造死按钮。
-->
<template>
  <div class="cost-batch-page">
    <t-tabs v-model="activeTab" class="page-tabs" data-testid="cb-tabs" @change="onTabChange">
      <t-tab-panel value="sheet" label="批次成本" data-testid="cb-tab-sheet" />
      <t-tab-panel value="wip" label="在制余额" data-testid="cb-tab-wip" />
    </t-tabs>

    <!-- ==================== ① 归集表（M5-02） ==================== -->
    <ListPage
      v-if="activeTab === 'sheet'"
      ref="sheetListRef"
      title="批次成本"
      meta-unit="批"
      testid="cb-sheet"
      table-testid="cb-sheet-table"
      meta-testid="cb-sheet-meta"
      btn-prefix="cb-sheet-btn"
      :columns="sheetColumns"
      :filters="sheetFilters"
      :filter-state="sheetFilter"
      :fetcher="sheetFetcher"
      :query-builder="sheetQueryBuilder"
      :page-size="PAGE_SIZE"
      @loaded="onSheetLoaded"
      @row-click="onSheetRowClick"
      @reset="onReset"
      @refresh="onRefresh"
    >
      <template #head-extra>
        <span class="panel-head__tip">
          一行一批：本期已归集的料 / 工 / 费。合计为**整张表**合计（不随翻页变），点批次号看明细
        </span>
      </template>

      <template #filter-period_code="{ value, set }">
        <t-date-picker
          :value="value"
          class="cb-month"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="会计期间（必填）"
          :clearable="false"
          data-testid="cb-sheet-filter-period"
          @change="(picked: unknown) => onPeriodChange(set, picked)"
        />
      </template>

      <template #object_no="{ row }">
        <t-link
          theme="primary"
          class="mono"
          :data-testid="`cb-sheet-row-${row.object_no}`"
          @click.stop="openDrawer(row.object_id, 'lines')"
        >
          {{ row.object_no }}
        </t-link>
      </template>
      <template #product_name="{ row }">
        <div class="name-cell">
          <span class="name-cell__main">{{ row.product_name || '—' }}</span>
          <span class="name-cell__sub">{{ row.product_code || '' }}{{ row.lot_no ? ` · 批号 ${row.lot_no}` : '' }}</span>
        </div>
      </template>
      <template #status="{ row }">
        <t-tag size="small" :theme="statusTheme(row.status)" variant="light">
          {{ statusLabel(row.status) }}
        </t-tag>
      </template>
      <template #mat="{ row }"><span class="money">{{ formatMoney(row.mat) }}</span></template>
      <template #labor="{ row }"><span class="money">{{ formatMoney(row.labor) }}</span></template>
      <template #oh="{ row }"><span class="money">{{ formatMoney(row.oh) }}</span></template>
      <template #total_amount="{ row }">
        <span class="money money--strong">{{ formatMoney(row.total_amount) }}</span>
      </template>
      <template #line_count="{ row }">
        <span class="num" :class="{ 'num--zero': row.line_count === 0 }">{{ row.line_count }}</span>
      </template>
      <template #last_occur_date="{ row }">
        <span class="mono">{{ dash(row.last_occur_date ? shortDate(row.last_occur_date) : '') }}</span>
      </template>
      <template #op="{ row }">
        <div class="row-ops">
          <t-link theme="primary" @click.stop="openDrawer(row.object_id, 'lines')">明细</t-link>
          <t-link theme="primary" @click.stop="openDrawer(row.object_id, 'sources')">来源</t-link>
          <t-link theme="primary" @click.stop="openDrawer(row.object_id, 'finish')">结转</t-link>
          <t-link theme="primary" @click.stop="openDrawer(row.object_id, 'ledger')">台账</t-link>
        </div>
      </template>

      <template #footer>
        <div class="totals" data-testid="cb-sheet-totals">
          <span class="totals__title">整表合计（{{ sheetTotals?.batch_count ?? 0 }} 批 · {{ sheetTotals?.line_count ?? 0 }} 行）</span>
          <span class="totals__item">材料 <b>{{ formatMoney(sheetTotals?.mat) }}</b></span>
          <span class="totals__item">人工 <b>{{ formatMoney(sheetTotals?.labor) }}</b></span>
          <span class="totals__item">制造费用 <b>{{ formatMoney(sheetTotals?.oh) }}</b></span>
          <span class="totals__item totals__item--strong">合计 <b>{{ formatMoney(sheetTotals?.total) }}</b></span>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ② 在制余额（M5-05） ==================== -->
    <ListPage
      v-else
      ref="wipListRef"
      title="在制余额"
      meta-unit="批"
      testid="cb-wip"
      table-testid="cb-wip-table"
      meta-testid="cb-wip-meta"
      btn-prefix="cb-wip-btn"
      :columns="wipColumns"
      :filters="wipFilters"
      :filter-state="wipFilter"
      :fetcher="wipFetcher"
      :query-builder="wipQueryBuilder"
      :page-size="PAGE_SIZE"
      @loaded="onWipLoaded"
      @row-click="onWipRowClick"
      @reset="onReset"
      @refresh="onRefresh"
    >
      <template #head-extra>
        <span class="panel-head__tip">
          截止本期的快照：在制余额 = 累计归集 − 已结转（逐要素，<b>不夹零</b>）；负数说明要查账
        </span>
      </template>

      <template #filter-period_code="{ value, set }">
        <t-date-picker
          :value="value"
          class="cb-month"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="截止期间（必填）"
          :clearable="false"
          data-testid="cb-wip-filter-period"
          @change="(picked: unknown) => onPeriodChange(set, picked)"
        />
      </template>

      <template #object_no="{ row }">
        <t-link
          theme="primary"
          class="mono"
          :data-testid="`cb-wip-row-${row.object_no}`"
          @click.stop="openDrawer(row.object_id, 'ledger')"
        >
          {{ row.object_no }}
        </t-link>
      </template>
      <template #product_name="{ row }">
        <div class="name-cell">
          <span class="name-cell__main">{{ row.product_name || '—' }}</span>
          <span class="name-cell__sub">{{ row.source_doc_no || '无生产订单' }}</span>
        </div>
      </template>
      <template #status="{ row }">
        <t-tag size="small" :theme="statusTheme(row.status)" variant="light">
          {{ statusLabel(row.status) }}
        </t-tag>
      </template>
      <template #balance_total="{ row }">
        <span class="money money--strong" :class="{ 'money--negative': row.negative }">
          {{ formatMoney(row.balance_total) }}
        </span>
      </template>
      <template #balance_mat="{ row }"><span class="money">{{ formatMoney(row.balance_mat) }}</span></template>
      <template #balance_labor="{ row }"><span class="money">{{ formatMoney(row.balance_labor) }}</span></template>
      <template #balance_oh="{ row }"><span class="money">{{ formatMoney(row.balance_oh) }}</span></template>
      <template #collected_total="{ row }"><span class="money">{{ formatMoney(row.collected_total) }}</span></template>
      <template #transferred_total="{ row }"><span class="money">{{ formatMoney(row.transferred_total) }}</span></template>
      <template #period_in_total="{ row }"><span class="money">{{ formatMoney(row.period_in_total) }}</span></template>
      <template #period_out_total="{ row }"><span class="money">{{ formatMoney(row.period_out_total) }}</span></template>
      <template #flags="{ row }">
        <div class="flag-cell">
          <t-tag v-if="row.has_balance" size="small" theme="warning" variant="light">有余额</t-tag>
          <t-tag v-if="row.negative" size="small" theme="danger" variant="light">余额为负</t-tag>
          <t-tag v-if="row.warnings.length > 0" size="small" theme="warning" variant="outline">
            警告 {{ row.warnings.length }}
          </t-tag>
          <span v-if="!row.has_balance && !row.negative && row.warnings.length === 0" class="muted">—</span>
        </div>
      </template>
      <template #op="{ row }">
        <div class="row-ops">
          <t-link theme="primary" @click.stop="openDrawer(row.object_id, 'ledger')">台账</t-link>
          <t-link theme="primary" @click.stop="openDrawer(row.object_id, 'finish')">结转</t-link>
        </div>
      </template>

      <template #footer>
        <div class="totals" data-testid="cb-wip-totals">
          <span class="totals__title">
            整表合计（{{ wipTotals?.batch_count ?? 0 }} 批，其中有余额 {{ wipTotals?.wip_batch_count ?? 0 }} 批）
          </span>
          <span class="totals__item">累计归集 <b>{{ formatMoney(wipTotals?.collected.total) }}</b></span>
          <span class="totals__item">已结转 <b>{{ formatMoney(wipTotals?.transferred.total) }}</b></span>
          <span class="totals__item">本期投入 <b>{{ formatMoney(wipTotals?.period_in.total) }}</b></span>
          <span class="totals__item">本期转出 <b>{{ formatMoney(wipTotals?.period_out.total) }}</b></span>
          <span
            class="totals__item totals__item--strong"
            :class="{ 'totals__item--negative': (wipTotals?.negative_batch_count ?? 0) > 0 }"
          >
            在制余额 <b>{{ formatMoney(wipTotals?.balance.total) }}</b>
            <template v-if="(wipTotals?.negative_batch_count ?? 0) > 0">
              （{{ wipTotals?.negative_batch_count }} 批为负）
            </template>
          </span>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ③ 批次抽屉（明细 / 来源 / 结转 / 台账） ==================== -->
    <t-drawer
      v-model:visible="drawerVisible"
      :header="drawerTitle"
      size="1120px"
      :footer="false"
      :close-on-overlay-click="false"
      destroy-on-close
      data-testid="cb-drawer"
    >
      <div class="batch-drawer">
        <t-loading :loading="drawerLoading" size="small" text="加载批次数据…" />

        <t-alert v-if="drawerError" theme="error" class="drawer-alert" data-testid="cb-drawer-error">
          <template #message><span class="pre">{{ drawerError }}</span></template>
        </t-alert>

        <!-- 批次概要（四处共用一份抬头，来源不同也不各画一套） -->
        <t-descriptions v-if="batchHead" :column="4" size="small" bordered class="batch-head">
          <t-descriptions-item label="批次号">
            <span class="mono">{{ batchHead.object_no }}</span>
            <t-tag size="small" :theme="statusTheme(batchHead.status)" variant="light" class="ml-8">
              {{ statusLabel(batchHead.status) }}
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="生产订单号">{{ dash(batchHead.source_doc_no) }}</t-descriptions-item>
          <t-descriptions-item label="产出品">
            {{ batchHead.product_code ? `${batchHead.product_code} ` : '' }}{{ batchHead.product_name || batchHead.product_id }}
          </t-descriptions-item>
          <t-descriptions-item label="产出批号">{{ dash(batchHead.lot_no) }}</t-descriptions-item>
          <t-descriptions-item label="车间">{{ dash(batchHead.workshop_dept_name || batchHead.workshop_dept_id) }}</t-descriptions-item>
          <t-descriptions-item label="计划 / 完工 / 在制">
            {{ trimDecimal(batchHead.plan_qty) }} / {{ trimDecimal(batchHead.completed_qty) }} /
            <b>{{ trimDecimal(batchHead.wip_qty) }}</b>
          </t-descriptions-item>
          <t-descriptions-item label="完工率">{{ pctText(batchHead.completion_pct) }}</t-descriptions-item>
          <t-descriptions-item label="期间">
            <span class="mono">{{ periodCode }}</span>
          </t-descriptions-item>
        </t-descriptions>

        <t-tabs v-model="drawerTab" class="drawer-tabs" data-testid="cb-drawer-tabs">
          <t-tab-panel value="lines" label="归集明细" data-testid="cb-drawer-tab-lines" />
          <t-tab-panel value="sources" label="来源与归集" data-testid="cb-drawer-tab-sources" />
          <t-tab-panel value="finish" label="完工结转" data-testid="cb-drawer-tab-finish" />
          <t-tab-panel value="ledger" label="在制台账" data-testid="cb-drawer-tab-ledger" />
        </t-tabs>

        <!-- -------------------- ① 归集明细 -------------------- -->
        <section v-if="drawerTab === 'lines'" class="pane" data-testid="cb-pane-lines">
          <t-alert v-if="linesError" theme="error" class="pane__alert">
            <template #message><span class="pre">{{ linesError }}</span></template>
          </t-alert>

          <div class="amount-strip" data-testid="cb-lines-totals">
            <span class="amount-strip__item">直接材料 <b>{{ formatMoney(detail?.totals.mat) }}</b></span>
            <span class="amount-strip__item">直接人工 <b>{{ formatMoney(detail?.totals.labor) }}</b></span>
            <span class="amount-strip__item">制造费用 <b>{{ formatMoney(detail?.totals.oh) }}</b></span>
            <span class="amount-strip__item amount-strip__item--strong">
              合计 <b>{{ formatMoney(detail?.totals.total) }}</b>（{{ detail?.lines.length ?? 0 }} 行）
            </span>
          </div>

          <t-table
            :data="detail?.lines ?? []"
            :columns="lineColumns"
            row-key="id"
            size="small"
            :hover="true"
            data-testid="cb-lines-table"
          >
            <template #element_label="{ row }">
              <t-tag size="small" variant="outline">{{ row.element_label }}</t-tag>
            </template>
            <template #amount="{ row }"><span class="money">{{ formatMoney(row.amount) }}</span></template>
            <template #qty="{ row }"><span class="num">{{ row.qty === null ? '—' : trimDecimal(row.qty) }}</span></template>
            <template #unit_price="{ row }">
              <span class="money">{{ row.unit_price === null ? '—' : formatMoney(row.unit_price) }}</span>
            </template>
            <template #material_name="{ row }">
              {{ row.material_code ? `${row.material_code} ` : '' }}{{ row.material_name || '—' }}
            </template>
            <template #occur_date="{ row }"><span class="mono">{{ row.occur_date }}</span></template>
            <template #empty>
              <div class="empty-tip" data-testid="cb-lines-empty">
                本期本批没有任何归集行 —— 料来自领料出库（`ISSUE` + 已过账）、工来自已确认工时台账、
                费来自直接挂本批的手工费用。点「来源与归集」看还差哪几笔。
              </div>
            </template>
          </t-table>
        </section>

        <!-- -------------------- ② 来源与归集 -------------------- -->
        <section v-else-if="drawerTab === 'sources'" class="pane" data-testid="cb-pane-sources">
          <t-alert
            v-if="sources && !sources.collectable"
            theme="warning"
            class="pane__alert"
            data-testid="cb-sources-block"
          >
            <template #message>
              <div class="pre">{{ sources.block_reason }}</div>
              <div class="pre muted">需要怎么做：{{ sources.block_fix }}</div>
            </template>
          </t-alert>

          <t-alert v-else-if="sources?.recalc_required" theme="info" class="pane__alert" data-testid="cb-sources-recalc">
            <template #message>
              本期已有草稿计算单：归集后要让它跟上，请到「成本核算」页重跑 calc（本页不自动重算，避免顺手改别人的批次）
            </template>
          </t-alert>

          <div class="pane__bar">
            <div class="pane__bar-text">
              <template v-if="sources">
                归集前合计 <b>{{ formatMoney(sources.totals.total) }}</b>
                ｜ 归集后合计 <b>{{ formatMoney(sources.after_totals.total) }}</b>
                <span v-if="sources.period_batch" class="muted">
                  ｜ 本期成本批次 {{ sources.period_batch.batch_no }}（{{ sources.period_batch.status_label }}）
                </span>
                <span v-else class="muted">｜ 本期尚未建成本批次（归集仍可写；calc 待建批次后再跑）</span>
              </template>
            </div>
            <t-button
              v-privilege="[PERM.write]"
              theme="primary"
              :loading="collecting"
              :disabled="!sources?.collectable || collecting"
              data-testid="cb-btn-collect"
              @click="doCollect"
            >
              <template #icon><t-icon name="import" /></template>
              归集本期（{{ periodCode }}）
            </t-button>
          </div>

          <div v-if="!sources?.collectable && sources?.block_reason" class="pane__blocked muted">
            {{ sources.block_fix }}
          </div>

          <t-alert v-if="collectResult" theme="success" class="pane__alert" data-testid="cb-collect-result">
            <template #message><span class="pre">{{ collectResult }}</span></template>
          </t-alert>

          <t-alert v-if="sourcesError" theme="error" class="pane__alert">
            <template #message><span class="pre">{{ sourcesError }}</span></template>
          </t-alert>

          <t-card
            v-for="element in COST_BATCH_ELEMENTS"
            :key="element"
            :bordered="true"
            class="element-card"
            :data-testid="`cb-element-${element}`"
          >
            <template #title>
              <span class="element-card__title">{{ elementLabel(element) }}</span>
            </template>
            <template #subtitle>
              <span class="element-card__sub">
                {{ elementSummary(element) }}
              </span>
            </template>

            <t-table
              :data="elementSourcesOf(sources, element)?.rows ?? []"
              :columns="sourceColumns"
              :row-key="(row: SourceLine) => `${row.source_type}:${row.source_id}`"
              size="small"
              :hover="true"
            >
              <template #source_no="{ row }">
                <span class="mono">{{ dash(row.source_no) }}</span>
              </template>
              <template #amount="{ row }"><span class="money">{{ formatMoney(row.amount) }}</span></template>
              <template #qty="{ row }">
                <span class="num">
                  {{ row.hours !== null ? `${trimDecimal(row.hours)} 工时` : row.qty === null ? '—' : trimDecimal(row.qty) }}
                </span>
              </template>
              <template #state="{ row }">
                <t-tag size="small" :theme="sourceStateTheme(row.state)" variant="light">
                  {{ row.state_label || sourceStateLabel(row.state) }}
                </t-tag>
              </template>
              <template #reason="{ row }">
                <span class="muted">{{ row.reason || '—' }}</span>
              </template>
              <template #empty>
                <div class="empty-tip">
                  这一路本期没有候选来源。{{ elementEmptyHint(element) }}
                </div>
              </template>
            </t-table>
          </t-card>
        </section>

        <!-- -------------------- ③ 完工结转 -------------------- -->
        <section v-else-if="drawerTab === 'finish'" class="pane" data-testid="cb-pane-finish">
          <t-alert v-if="previewError" theme="error" class="pane__alert">
            <template #message><span class="pre">{{ previewError }}</span></template>
          </t-alert>

          <t-alert
            v-if="preview && preview.blockers.length > 0"
            theme="warning"
            class="pane__alert"
            data-testid="cb-finish-blockers"
          >
            <template #message>
              <div class="blocker-head">这批现在不能结转，{{ preview.blockers.length }} 项前置未满足：</div>
              <ol class="blocker-list">
                <li v-for="(text, index) in blockerLines(preview)" :key="index" class="pre">{{ text }}</li>
              </ol>
            </template>
          </t-alert>

          <t-alert
            v-else-if="preview && !preview.transferable"
            theme="info"
            class="pane__alert"
            data-testid="cb-finish-nothing"
          >
            <template #message>
              本次可结转金额是 0（累计归集已全部转出）。分批法的结转是**差额口径**，
              再点一次只会得到一次幂等空转 —— 无需操作。
            </template>
          </t-alert>

          <t-alert v-else-if="preview" theme="success" class="pane__alert" data-testid="cb-finish-ready">
            <template #message>{{ finishSummaryText(preview) }}</template>
          </t-alert>

          <t-descriptions v-if="preview" :column="4" size="small" bordered>
            <t-descriptions-item label="累计归集">
              {{ formatMoney(preview.collected.total) }}
              <span class="muted">（料 {{ formatMoney(preview.collected.material) }} / 工 {{ formatMoney(preview.collected.labor) }} / 费 {{ formatMoney(preview.collected.overhead) }}）</span>
            </t-descriptions-item>
            <t-descriptions-item label="已结转">
              {{ formatMoney(preview.transferred.total) }}
              <span class="muted">（{{ preview.history.length }} 次）</span>
            </t-descriptions-item>
            <t-descriptions-item label="本次待结转">
              <b>{{ formatMoney(preview.pending.total) }}</b>
              <span class="muted">（料 {{ formatMoney(preview.pending.material) }} / 工 {{ formatMoney(preview.pending.labor) }} / 费 {{ formatMoney(preview.pending.overhead) }}）</span>
            </t-descriptions-item>
            <t-descriptions-item label="完工数量">{{ trimDecimal(preview.finished_qty) }}</t-descriptions-item>
            <t-descriptions-item label="本次单位成本">{{ unitCostText(preview.unit_cost) }}</t-descriptions-item>
            <t-descriptions-item label="结转后累计单位成本">{{ unitCostText(preview.cumulative_unit_cost) }}</t-descriptions-item>
            <t-descriptions-item label="本次序号">第 {{ preview.transfer_no }} 次</t-descriptions-item>
            <t-descriptions-item label="期间成本批次">
              <template v-if="preview.period_batch">
                {{ preview.period_batch.batch_no }}（{{ preview.period_batch.status_label }}）
              </template>
              <span v-else class="muted">本期未建</span>
            </t-descriptions-item>
          </t-descriptions>

          <t-alert v-if="preview?.settled_in_books" theme="warning" class="pane__alert" data-testid="cb-finish-settled">
            <template #message>
              本批已有「已结算」计算单：账务已经走过（完工入库单 + 结转凭证）。要改数请先红冲该期间结算，再按新口径重结
            </template>
          </t-alert>

          <div v-if="preview && preview.warnings.length > 0" class="warn-list" data-testid="cb-finish-warnings">
            <div v-for="(text, index) in preview.warnings" :key="index" class="warn-list__item">
              <t-icon name="info-circle" />
              <span class="pre">{{ text }}</span>
            </div>
          </div>

          <div class="pane__bar">
            <t-input
              v-model="finishRemark"
              class="pane__remark"
              placeholder="结转备注（可选，写进结转单留痕）"
              :maxlength="200"
              data-testid="cb-finish-remark"
            />
            <t-tooltip :content="finishDisabledReason(preview)" :disabled="canFinish(preview)">
              <span>
                <t-button
                  v-privilege="[PERM.write]"
                  theme="primary"
                  :loading="finishing"
                  :disabled="!canFinish(preview) || finishing"
                  data-testid="cb-btn-finish"
                  @click="doFinish"
                >
                  <template #icon><t-icon name="check" /></template>
                  结转（{{ preview ? formatMoney(preview.pending.total) : '—' }}）
                </t-button>
              </span>
            </t-tooltip>
          </div>

          <t-alert v-if="finishResult" theme="success" class="pane__alert" data-testid="cb-finish-result">
            <template #message><span class="pre">{{ finishResult }}</span></template>
          </t-alert>

          <div class="sub-title">历史结转单（{{ preview?.history.length ?? 0 }} 张）</div>
          <t-table
            :data="preview?.history ?? []"
            :columns="historyColumns"
            row-key="id"
            size="small"
            :hover="true"
            data-testid="cb-finish-history"
          >
            <template #amount_total="{ row }"><span class="money">{{ formatMoney(row.amounts.total) }}</span></template>
            <template #unit_cost="{ row }"><span class="money">{{ unitCostText(row.unit_cost) }}</span></template>
            <template #finished_qty="{ row }"><span class="num">{{ trimDecimal(row.finished_qty) }}</span></template>
            <template #created_at="{ row }"><span class="mono">{{ row.created_at }}</span></template>
            <template #created_by_name="{ row }">{{ row.created_by_name || '—' }}</template>
            <template #empty>
              <div class="empty-tip">这批还一次都没结转（分批法：整批完工后才转，部分完工一分不转）</div>
            </template>
          </t-table>
        </section>

        <!-- -------------------- ④ 在制台账 -------------------- -->
        <section v-else class="pane" data-testid="cb-pane-ledger">
          <t-alert v-if="ledgerError" theme="error" class="pane__alert">
            <template #message><span class="pre">{{ ledgerError }}</span></template>
          </t-alert>

          <t-alert
            v-if="ledgerMismatches(ledger).length > 0"
            theme="error"
            class="pane__alert"
            data-testid="cb-ledger-mismatch"
          >
            <template #message>
              <div class="blocker-head">台账自身对不上，先别信这张表（后端数据或口径有问题，请报障）：</div>
              <div v-for="(text, index) in ledgerMismatches(ledger)" :key="index" class="pre">{{ text }}</div>
            </template>
          </t-alert>

          <div class="amount-strip" data-testid="cb-ledger-summary">
            <span class="amount-strip__item muted">{{ ledgerOpeningText(ledger) }}</span>
            <span class="amount-strip__item">累计归集 <b>{{ formatMoney(ledger?.summary.collected.total) }}</b></span>
            <span class="amount-strip__item">已结转 <b>{{ formatMoney(ledger?.summary.transferred.total) }}</b></span>
            <span
              class="amount-strip__item amount-strip__item--strong"
              :class="{ 'amount-strip__item--negative': wipAmountsNegative(ledger?.summary.balance ?? EMPTY_WIP) }"
            >
              在制余额 <b>{{ formatMoney(ledger?.summary.balance.total) }}</b>
            </span>
            <span class="amount-strip__item muted">
              {{ ledger?.summary.collect_lines ?? 0 }} 行归集 · {{ ledger?.summary.transfer_times ?? 0 }} 次结转
            </span>
          </div>

          <t-alert v-if="ledger?.after_period" theme="info" class="pane__alert" data-testid="cb-ledger-after">
            <template #message>
              这是「截至 {{ ledger?.period_code }}」的快照：之后还有归集 {{ formatMoney(ledger?.after_period.collected_total) }} /
              结转 {{ formatMoney(ledger?.after_period.transferred_total) }}。要看最新余额请把期间往后调
            </template>
          </t-alert>

          <t-alert v-if="ledger?.settled" theme="info" class="pane__alert" data-testid="cb-ledger-settled">
            <template #message>
              本批已随期间结算（{{ ledger?.settled?.period_code }}）：完工转出
              {{ formatMoney(ledger?.settled?.finished_cost) }}，期末在制（约当产量口径）
              {{ formatMoney(ledger?.settled?.wip_end_cost) }}。两个口径本来就不该逐分相等
            </template>
          </t-alert>

          <t-table
            :data="ledgerRows"
            :columns="ledgerColumns"
            row-key="period_code"
            size="small"
            :hover="true"
            data-testid="cb-ledger-table"
          >
            <template #opening_total="{ row }"><span class="money">{{ formatMoney(row.opening_total) }}</span></template>
            <template #inflow_total="{ row }"><span class="money">{{ formatMoney(row.inflow_total) }}</span></template>
            <template #outflow_total="{ row }"><span class="money">{{ formatMoney(row.outflow_total) }}</span></template>
            <template #closing_total="{ row }">
              <span class="money money--strong">{{ formatMoney(row.closing_total) }}</span>
            </template>
            <template #empty>
              <div class="empty-tip">截止本期间既没有归集也没有结转（余额表同样会是 0）</div>
            </template>
          </t-table>

          <div v-if="ledger && ledger.warnings.length > 0" class="warn-list" data-testid="cb-ledger-warnings">
            <div v-for="item in ledger.warnings" :key="item.code" class="warn-list__item">
              <t-tag size="small" :theme="warningTheme(item.level)" variant="light">{{ item.code }}</t-tag>
              <span class="pre">{{ item.message }}（{{ item.hint }}）</span>
            </div>
          </div>

          <div v-if="ledger && ledger.notes.length > 0" class="notes" data-testid="cb-ledger-notes">
            <div class="notes__title">口径说明</div>
            <div v-for="(text, index) in ledger.notes" :key="index" class="notes__item pre">{{ text }}</div>
          </div>
        </section>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 批次成本页脚本。
 *
 * 分工：本文件只做「拉数 + 串请求 + 画」；入参拼装、金额相加、三态主题、闸门文案、
 * 台账自洽校验全在 `cost-batch-logic.ts`（有单测 `tests/unit/erp-cost-batch.test.ts`）。
 */
import { computed, nextTick, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';

import {
  COST_BATCH_ELEMENTS,
  COST_BATCH_STATUSES,
  COST_BATCH_STATUS_LABELS,
  SHEET_SORTS,
  WIP_SORTS,
  collectBatch,
  fetchBatchFinishPreview,
  fetchBatchSources,
  fetchCollectSheetDetail,
  fetchCollectSheetPage,
  fetchWipBalance,
  fetchWipLedger,
  finishBatch,
  type BatchFinishPreview,
  type BatchSources,
  type CostBatchElement,
  type CostBatchStatus,
  type SheetBatch,
  type SheetDetail,
  type SheetTotals,
  type SourceLine,
  type WipLedger,
} from '@/api/business/erp/cost-batch';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import { errorText } from '@/views/business/erp/shared/error-text';
import { dash, formatMoney, isMonthKey, shortDate, todayPeriod } from '@/views/business/erp/shared/money';

import {
  blockerLines,
  buildSheetQuery,
  buildWipQuery,
  canFinish,
  collectResultText,
  elementLabel,
  elementSourcesOf,
  elementSummaryText,
  emptySheetFilter,
  emptyWipFilter,
  finishDisabledReason,
  finishSummaryText,
  flattenLedgerRows,
  flattenSheetRow,
  flattenWipRow,
  ledgerMismatches,
  ledgerOpeningText,
  pctText,
  periodOrFallback,
  sourceStateLabel,
  sourceStateTheme,
  statusLabel,
  statusTheme,
  trimDecimal,
  unitCostText,
  warningTheme,
  wipAmountsNegative,
  type SheetFilterState,
  type SheetRowView,
  type WipFilterState,
  type WipRowView,
} from './cost-batch-logic';

/** 本页两个权限码（都是复用码，见 `cost-batch-kinds.ts::COST_BATCH_PERMISSIONS`） */
const PERM = { read: 'business:erp:cost-object:list', write: 'business:erp:cost-period:calc' } as const;

/** 每页条数（与成本域其余页一致） */
const PAGE_SIZE = 20;

/** 期间记忆键：期间必填且每月都要切，记住了才不用天天重选 */
const PERIOD_MEMORY_KEY = 'cloudpivot.cost-batch.period';

/** 空余额（模板里比较负数时的占位，避免 `?.` 链后传 `undefined`） */
const EMPTY_WIP = { material: '0', labor: '0', overhead: '0', total: '0' } as const;

// ==================== 页签 ====================

/** 当前页签：`sheet` 归集表 / `wip` 在制余额 */
const activeTab = ref<'sheet' | 'wip'>('sheet');

/** 抽屉里当前子页签 */
const drawerTab = ref<'lines' | 'sources' | 'finish' | 'ledger'>('lines');

const sheetListRef = ref<InstanceType<typeof ListPage> | null>(null);
const wipListRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 当前页签对应的列表实例（期间切换后重查用） */
const activeListRef = computed(() => (activeTab.value === 'sheet' ? sheetListRef.value : wipListRef.value));

/** 切页签：两个列表各自首次进入时才拉，切回来重新搜一次（期间可能已变） */
function onTabChange(): void {
  void nextTick(() => {
    void activeListRef.value?.search();
  });
}

// ==================== 期间（必填，记住上次选择） ====================

/** 读记忆的期间（不合形状 / 读不到 → 本月） */
function readRememberedPeriod(): string {
  try {
    const saved = window.localStorage.getItem(PERIOD_MEMORY_KEY) ?? '';
    return isMonthKey(saved) ? saved : todayPeriod();
  } catch {
    return todayPeriod();
  }
}

/** 记住本次期间（隐私模式下 localStorage 会抛，吞掉即可） */
function rememberPeriod(period: string): void {
  try {
    window.localStorage.setItem(PERIOD_MEMORY_KEY, period);
  } catch {
    /* 记忆失败不影响主流程 */
  }
}

const initialPeriod = readRememberedPeriod();

/** 当前抽屉 / 两个列表共用的期间 */
const periodCode = ref(initialPeriod);

// ==================== ① 归集表 ====================

const sheetFilter = reactive<SheetFilterState>({ ...emptySheetFilter(), period_code: initialPeriod });
const sheetTotals = ref<SheetTotals | null>(null);
const sheetRows = ref<SheetRowView[]>([]);

/** 期间变更：两个列表的期间同步 + 记住 + 重查当前页签 */
function onPeriodChange(set: (value: unknown) => void, picked: unknown): void {
  const next = periodOrFallback(typeof picked === 'string' ? picked : '');
  set(next);
  sheetFilter.period_code = next;
  wipFilter.period_code = next;
  periodCode.value = next;
  rememberPeriod(next);
  void nextTick(() => {
    void activeListRef.value?.search();
  });
}

/**
 * 「重置」：`ListPage` 会把 `custom` 期间筛选写回 `defaultValue`（进页面时那个月），
 * 这里把两个列表与抽屉的期间重新对齐 —— 否则切到另一页签还带着旧期间，
 * 看到的数与刚重置的不是同一期。
 */
function onReset(): void {
  sheetFilter.period_code = periodOrFallback(sheetFilter.period_code);
  wipFilter.period_code = sheetFilter.period_code;
  periodCode.value = sheetFilter.period_code;
}

const sheetFetcher: ListFetcher<SheetRowView> = async (query) => {
  const data = await fetchCollectSheetPage(query as Parameters<typeof fetchCollectSheetPage>[0]);
  sheetTotals.value = data.totals;
  return {
    list: data.list.map(flattenSheetRow),
    total: data.total,
    page: data.page,
    page_size: data.page_size,
  };
};

const sheetQueryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildSheetQuery(state as SheetFilterState, page, pageSize);

function onSheetLoaded(payload: { rows: SheetRowView[] }): void {
  sheetRows.value = payload.rows;
}

function onSheetRowClick(payload: { row: SheetRowView }): void {
  openDrawer(payload.row.object_id, 'lines');
}

const sheetColumns: ColumnDef[] = [
  { colKey: 'object_no', title: '批次号', width: 152, fixed: 'left', ellipsis: true },
  { colKey: 'source_doc_no', title: '生产订单号', width: 132, ellipsis: true },
  { colKey: 'product_name', title: '产出品', width: 168, ellipsis: true },
  { colKey: 'workshop_dept_name', title: '车间', width: 92, ellipsis: true },
  { colKey: 'plan_qty_text', title: '计划', width: 76, align: 'right' },
  { colKey: 'completed_qty_text', title: '完工', width: 76, align: 'right' },
  { colKey: 'wip_qty_text', title: '在制', width: 76, align: 'right' },
  { colKey: 'pct_text', title: '完工率', width: 80, align: 'right' },
  { colKey: 'status', title: '状态', width: 84 },
  { colKey: 'mat', title: '直接材料', width: 112, align: 'right' },
  { colKey: 'labor', title: '直接人工', width: 112, align: 'right' },
  { colKey: 'oh', title: '制造费用', width: 112, align: 'right' },
  { colKey: 'total_amount', title: '合计', width: 122, align: 'right' },
  { colKey: 'line_count', title: '归集行', width: 76, align: 'right' },
  { colKey: 'last_occur_date', title: '最后归集', width: 96 },
  { colKey: 'op', title: '操作', width: 176, fixed: 'right' },
];

// ==================== ② 在制余额 ====================

const wipFilter = reactive<WipFilterState>({ ...emptyWipFilter(), period_code: initialPeriod });
const wipTotals = ref<Awaited<ReturnType<typeof fetchWipBalance>>['totals'] | null>(null);
const wipRows = ref<WipRowView[]>([]);

const wipFetcher: ListFetcher<WipRowView> = async (query) => {
  const data = await fetchWipBalance(query as Parameters<typeof fetchWipBalance>[0]);
  wipTotals.value = data.totals;
  return {
    list: data.list.map(flattenWipRow),
    total: data.total,
    page: data.page,
    page_size: data.page_size,
  };
};

const wipQueryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildWipQuery(state as WipFilterState, page, pageSize);

function onWipLoaded(payload: { rows: WipRowView[] }): void {
  wipRows.value = payload.rows;
}

function onWipRowClick(payload: { row: WipRowView }): void {
  openDrawer(payload.row.object_id, 'ledger');
}

const wipColumns: ColumnDef[] = [
  { colKey: 'object_no', title: '批次号', width: 152, fixed: 'left', ellipsis: true },
  { colKey: 'product_name', title: '产出品 / 订单', width: 176, ellipsis: true },
  { colKey: 'workshop_dept_name', title: '车间', width: 92, ellipsis: true },
  { colKey: 'status', title: '状态', width: 84 },
  { colKey: 'wip_qty_text', title: '在制数量', width: 88, align: 'right' },
  { colKey: 'collected_total', title: '累计归集', width: 118, align: 'right' },
  { colKey: 'transferred_total', title: '已结转', width: 118, align: 'right' },
  { colKey: 'balance_mat', title: '余额·料', width: 108, align: 'right' },
  { colKey: 'balance_labor', title: '余额·工', width: 108, align: 'right' },
  { colKey: 'balance_oh', title: '余额·费', width: 108, align: 'right' },
  { colKey: 'balance_total', title: '在制余额', width: 122, align: 'right' },
  { colKey: 'period_in_total', title: '本期投入', width: 112, align: 'right' },
  { colKey: 'period_out_total', title: '本期转出', width: 112, align: 'right' },
  { colKey: 'flags', title: '提示', width: 150 },
  { colKey: 'period_span', title: '发生期间', width: 148, ellipsis: true },
  { colKey: 'op', title: '操作', width: 120, fixed: 'right' },
];

// ==================== 筛选（两个列表共用一份选项） ====================

/** 部门树 → 车间下拉（谁都不能手打 uuid） */
const deptTree = ref<DeptNode[]>([]);

const deptOptions = computed(() => flattenDeptTree(deptTree.value));

function flattenDeptTree(nodes: DeptNode[], depth = 0): Array<{ label: string; value: string }> {
  const out: Array<{ label: string; value: string }> = [];
  for (const node of nodes) {
    out.push({ label: `${'　'.repeat(depth)}${node.dept_name}`, value: node.id });
    out.push(...flattenDeptTree(node.children ?? [], depth + 1));
  }
  return out;
}

const statusOptions = COST_BATCH_STATUSES.map((status: CostBatchStatus) => ({
  label: COST_BATCH_STATUS_LABELS[status],
  value: status,
}));

const sheetSortOptions = SHEET_SORTS.map((item) => ({ label: `按${item.label}`, value: item.value }));
const wipSortOptions = WIP_SORTS.map((item) => ({ label: `按${item.label}`, value: item.value }));

const onlyWipOptions = [
  { label: '仅有余额的批', value: '1' },
  { label: '全部批次（含已转完）', value: '' },
];

const sheetFilters = computed<FilterDef[]>(() => [
  { key: 'period_code', type: 'custom', defaultValue: initialPeriod, testid: 'cb-sheet-filter-period' },
  { key: 'status', type: 'select', options: statusOptions, searchOnChange: true, placeholder: '全部状态', width: 120, testid: 'cb-sheet-filter-status' },
  { key: 'workshop_dept_id', type: 'select', options: deptOptions.value, filterable: true, searchOnChange: true, placeholder: '全部车间', width: 150, testid: 'cb-sheet-filter-dept' },
  { key: 'sort', type: 'select', options: sheetSortOptions, searchOnChange: true, placeholder: '按合计金额', width: 140, testid: 'cb-sheet-filter-sort' },
  { key: 'keyword', type: 'keyword', placeholder: '批次号 / 订单号 / 批号 / 产出品', width: 240, testid: 'cb-sheet-filter-keyword' },
]);

const wipFilters = computed<FilterDef[]>(() => [
  { key: 'period_code', type: 'custom', defaultValue: initialPeriod, testid: 'cb-wip-filter-period' },
  { key: 'status', type: 'select', options: statusOptions, searchOnChange: true, placeholder: '全部状态', width: 120, testid: 'cb-wip-filter-status' },
  { key: 'only_wip', type: 'select', options: onlyWipOptions, searchOnChange: true, placeholder: '全部批次', width: 170, testid: 'cb-wip-filter-only' },
  { key: 'workshop_dept_id', type: 'select', options: deptOptions.value, filterable: true, searchOnChange: true, placeholder: '全部车间', width: 150, testid: 'cb-wip-filter-dept' },
  { key: 'sort', type: 'select', options: wipSortOptions, searchOnChange: true, placeholder: '按在制余额', width: 140, testid: 'cb-wip-filter-sort' },
  { key: 'keyword', type: 'keyword', placeholder: '批次号 / 订单号 / 批号 / 产出品', width: 240, testid: 'cb-wip-filter-keyword' },
]);

// ==================== 抽屉（批次明细） ====================

const drawerVisible = ref(false);
const drawerLoading = ref(false);
const drawerError = ref('');
const objectId = ref('');
const batchHead = ref<SheetBatch | null>(null);

const detail = ref<SheetDetail | null>(null);
const sources = ref<BatchSources | null>(null);
const preview = ref<BatchFinishPreview | null>(null);
const ledger = ref<WipLedger | null>(null);

const linesError = ref('');
const sourcesError = ref('');
const previewError = ref('');
const ledgerError = ref('');

const collecting = ref(false);
const finishing = ref(false);
const finishRemark = ref('');
const collectResult = ref('');
const finishResult = ref('');

const drawerTitle = computed(() => {
  const head = batchHead.value;
  if (!head) {
    return '批次成本明细';
  }
  const product = head.product_name || head.product_code || '';
  return `${head.object_no}${product ? ` · ${product}` : ''}（${periodCode.value}）`;
});

const ledgerRows = computed(() => flattenLedgerRows(ledger.value));

/** 打开抽屉：先清空上一批的状态，再并行拉四处数据 */
function openDrawer(id: string, tab: 'lines' | 'sources' | 'finish' | 'ledger'): void {
  objectId.value = id;
  drawerTab.value = tab;
  drawerVisible.value = true;
  batchHead.value = null;
  detail.value = null;
  sources.value = null;
  preview.value = null;
  ledger.value = null;
  linesError.value = '';
  sourcesError.value = '';
  previewError.value = '';
  ledgerError.value = '';
  drawerError.value = '';
  collectResult.value = '';
  finishResult.value = '';
  finishRemark.value = '';
  void loadAll();
}

/** 四处并行：任一处失败只影响它自己那块（其余照常显示，报错带请求 ID） */
async function loadAll(): Promise<void> {
  drawerLoading.value = true;
  try {
    await Promise.all([loadDetail(), loadSources(), loadPreview(), loadLedger()]);
  } finally {
    drawerLoading.value = false;
  }
}

async function loadDetail(): Promise<void> {
  if (!objectId.value) {
    return;
  }
  try {
    const data = await fetchCollectSheetDetail(objectId.value, periodCode.value);
    detail.value = data;
    batchHead.value = data.batch;
    linesError.value = '';
  } catch (error) {
    linesError.value = errorText(error, '归集明细加载失败');
  }
}

async function loadSources(): Promise<void> {
  if (!objectId.value) {
    return;
  }
  try {
    sources.value = await fetchBatchSources(objectId.value, periodCode.value);
    if (!batchHead.value) {
      batchHead.value = sources.value.batch;
    }
    sourcesError.value = '';
  } catch (error) {
    sourcesError.value = errorText(error, '三路来源加载失败');
  }
}

async function loadPreview(): Promise<void> {
  if (!objectId.value) {
    return;
  }
  try {
    preview.value = await fetchBatchFinishPreview(objectId.value, periodCode.value);
    if (!batchHead.value) {
      batchHead.value = preview.value.batch;
    }
    previewError.value = '';
  } catch (error) {
    previewError.value = errorText(error, '结转试算加载失败');
  }
}

async function loadLedger(): Promise<void> {
  if (!objectId.value) {
    return;
  }
  try {
    ledger.value = await fetchWipLedger(objectId.value, periodCode.value);
    if (!batchHead.value) {
      batchHead.value = ledger.value.batch;
    }
    ledgerError.value = '';
  } catch (error) {
    ledgerError.value = errorText(error, '在制台账加载失败');
  }
}

/** 拉两个列表的当前页（归集 / 结转后要看见新数字） */
async function reloadLists(): Promise<void> {
  await Promise.all([sheetListRef.value?.reload(), wipListRef.value?.reload()]);
}

/** 「刷新」按钮：列表由 ListPage 自身重拉，这里补抽屉与部门字典 */
function onRefresh(): void {
  void loadDepts();
  if (drawerVisible.value && objectId.value) {
    void loadAll();
  }
}

// ==================== 动作：归集 ====================

/** 二次确认（归集会写账，不能一键直落） */
function confirmAction(options: {
  header: string;
  body: string;
  confirmBtn: string;
  theme?: 'warning' | 'danger';
}): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    const dialog = DialogPlugin.confirm({
      header: options.header,
      body: options.body,
      theme: options.theme ?? 'warning',
      confirmBtn: options.confirmBtn,
      onConfirm: () => {
        dialog.hide();
        resolve(true);
      },
      onClose: () => resolve(false),
      onCancel: () => resolve(false),
    });
  });
}

async function doCollect(): Promise<void> {
  const head = batchHead.value;
  if (!head || !sources.value) {
    return;
  }
  const confirmed = await confirmAction({
    header: '确认归集本期',
    body:
      `把 ${head.object_no} 在 ${periodCode.value} 的三路来源（领料 / 工时 / 直接费用）归集进本批。\n` +
      `本次将写：材料 ${sources.value.material.pending_rows} 行 · 人工 ${sources.value.labor.pending_rows} 行 · ` +
      `制造费用 ${sources.value.overhead.pending_rows} 行（金额由来源单据决定，页面不能填）。`,
    confirmBtn: '确认归集',
  });
  if (!confirmed) {
    return;
  }
  collecting.value = true;
  collectResult.value = '';
  try {
    const result = await collectBatch(head.object_id, periodCode.value);
    collectResult.value = collectResultText(result);
    MessagePlugin.success(`${result.batch.object_no} 已归集（${periodCode.value}）`);
    await Promise.all([loadDetail(), loadSources(), loadPreview(), loadLedger(), reloadLists()]);
  } catch (error) {
    sourcesError.value = errorText(error, '归集失败');
  } finally {
    collecting.value = false;
  }
}

// ==================== 动作：结转 ====================

async function doFinish(): Promise<void> {
  const head = batchHead.value;
  const current = preview.value;
  if (!head || !current || !canFinish(current)) {
    return;
  }
  const confirmed = await confirmAction({
    header: '确认结转',
    body:
      `${finishSummaryText(current)}\n` +
      '分批法按批结转：结转后再归集的钱要等下一次结转才转出，已过账的凭证不能改（只能红冲）。',
    confirmBtn: '确认结转',
  });
  if (!confirmed) {
    return;
  }
  finishing.value = true;
  finishResult.value = '';
  try {
    const result = await finishBatch(head.object_id, periodCode.value, finishRemark.value);
    finishResult.value = result.inserted
      ? `${result.message}\n本次结转 料 ${formatMoney(result.transfer.material)} / 工 ${formatMoney(result.transfer.labor)} / ` +
        `费 ${formatMoney(result.transfer.overhead)} / 合计 ${formatMoney(result.transfer.total)}，结转后剩余 ${formatMoney(result.remaining.total)}`
      : `${result.message}（幂等空转，未写新单）`;
    preview.value = result.preview;
    MessagePlugin.success(result.inserted ? '已结转' : '本次无可结转金额');
    finishRemark.value = '';
    await Promise.all([loadDetail(), loadSources(), loadLedger(), reloadLists()]);
  } catch (error) {
    previewError.value = errorText(error, '结转失败');
  } finally {
    finishing.value = false;
  }
}

// ==================== 表格列（明细 / 来源 / 历史 / 台账） ====================

const lineColumns: ColumnDef[] = [
  { colKey: 'element_label', title: '要素', width: 96 },
  { colKey: 'source_type_label', title: '来源', width: 100 },
  { colKey: 'source_no', title: '来源单号', width: 150, ellipsis: true },
  { colKey: 'occur_date', title: '发生日期', width: 108 },
  { colKey: 'material_name', title: '物料', width: 170, ellipsis: true },
  { colKey: 'qty', title: '数量', width: 88, align: 'right' },
  { colKey: 'unit_price', title: '单价', width: 100, align: 'right' },
  { colKey: 'amount', title: '金额', width: 118, align: 'right' },
  { colKey: 'dept_name', title: '部门', width: 110, ellipsis: true },
  { colKey: 'remark', title: '备注', width: 160, ellipsis: true },
];

const sourceColumns: ColumnDef[] = [
  { colKey: 'source_no', title: '来源单号', width: 150, ellipsis: true },
  { colKey: 'label', title: '类型', width: 110, ellipsis: true },
  { colKey: 'occur_date', title: '发生日期', width: 108 },
  { colKey: 'qty', title: '数量 / 工时', width: 108, align: 'right' },
  { colKey: 'amount', title: '金额', width: 116, align: 'right' },
  { colKey: 'state', title: '状态', width: 100 },
  { colKey: 'reason', title: '说明', width: 260, ellipsis: true },
];

const historyColumns: ColumnDef[] = [
  { colKey: 'transfer_seq', title: '第 n 次', width: 80, align: 'right' },
  { colKey: 'period_code', title: '期间', width: 100 },
  { colKey: 'finished_qty', title: '完工数量', width: 100, align: 'right' },
  { colKey: 'amount_total', title: '结转金额', width: 120, align: 'right' },
  { colKey: 'unit_cost', title: '单位成本', width: 110, align: 'right' },
  { colKey: 'created_at', title: '结转时间', width: 160 },
  { colKey: 'created_by_name', title: '操作人', width: 110 },
  { colKey: 'remark', title: '备注', width: 180, ellipsis: true },
];

const ledgerColumns: ColumnDef[] = [
  { colKey: 'period_code', title: '期间', width: 100 },
  { colKey: 'opening_total', title: '期初余额', width: 128, align: 'right' },
  { colKey: 'inflow_total', title: '本期投入（归集）', width: 150, align: 'right' },
  { colKey: 'outflow_total', title: '本期转出（结转）', width: 150, align: 'right' },
  { colKey: 'closing_total', title: '期末余额', width: 128, align: 'right' },
  { colKey: 'collect_lines', title: '归集行', width: 84, align: 'right' },
  { colKey: 'transfer_times', title: '结转次数', width: 92, align: 'right' },
];

// ==================== 初始化 ====================

/** 部门字典（车间筛选下拉） */
async function loadDepts(): Promise<void> {
  try {
    deptTree.value = await getDeptTree({ status: 1 });
  } catch {
    deptTree.value = [];
  }
}

onMounted(() => {
  void loadDepts();
});

/** 某一路要素的摘要文案（三张卡片共用；未加载时给「加载中」而不是空串） */
function elementSummary(element: CostBatchElement): string {
  const leg = elementSourcesOf(sources.value, element);
  return leg ? elementSummaryText(leg) : '加载中…';
}

/** 某个要素本期没有候选来源时，告诉用户「该去哪补」 */
function elementEmptyHint(element: CostBatchElement): string {
  if (element === 'MATERIAL') {
    return '料来自领料出库单（out_type=ISSUE、已过账、且单头挂在本批）。先看领料单是否已过账、是否挂对了批次。';
  }
  if (element === 'LABOR') {
    return '工来自工时台账（status=CONFIRMED、本期、对象为本批）。缺费率的行会进「不可归集」，先补费率。';
  }
  return '只收「直接挂在本批」的手工费用；折旧 / 报销 / 未挂对象的费用进车间费用池，由分摊段处理。';
}
</script>

<style scoped>
.cost-batch-page {
  padding: 12px;
}

.page-tabs {
  margin-bottom: 4px;
}

.panel-head__tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.cb-month {
  width: 170px;
}

.mono {
  font-family: var(--td-font-family-number, monospace);
}

.money {
  font-variant-numeric: tabular-nums;
}

.money--strong {
  font-weight: 600;
}

.money--negative {
  color: var(--td-error-color);
}

.num {
  font-variant-numeric: tabular-nums;
}

.num--zero {
  color: var(--td-text-color-placeholder);
}

.muted {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.ml-8 {
  margin-left: 8px;
}

.pre {
  white-space: pre-wrap;
  word-break: break-word;
}

.name-cell {
  display: flex;
  flex-direction: column;
  line-height: 1.3;
}

.name-cell__main {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.name-cell__sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.row-ops {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
}

.flag-cell {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.totals {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 16px;
  padding: 10px 4px 2px;
  border-top: 1px dashed var(--td-component-stroke);
  font-size: 13px;
}

.totals__title {
  color: var(--td-text-color-secondary);
}

.totals__item b {
  font-variant-numeric: tabular-nums;
}

.totals__item--strong {
  margin-left: auto;
  font-weight: 600;
}

.totals__item--negative b {
  color: var(--td-error-color);
}

.batch-drawer {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.drawer-alert,
.pane__alert {
  margin-bottom: 4px;
}

.batch-head {
  margin-bottom: 4px;
}

.drawer-tabs {
  margin-top: 4px;
}

.pane {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.pane__bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.pane__bar-text {
  font-size: 13px;
}

.pane__remark {
  max-width: 420px;
}

.pane__blocked {
  margin-top: -6px;
}

.amount-strip {
  display: flex;
  flex-wrap: wrap;
  gap: 18px;
  padding: 10px 12px;
  background: var(--td-bg-color-container-hover);
  border-radius: 6px;
  font-size: 13px;
}

.amount-strip__item b {
  font-variant-numeric: tabular-nums;
}

.amount-strip__item--strong {
  margin-left: auto;
  font-weight: 600;
}

.amount-strip__item--negative b {
  color: var(--td-error-color);
}

.element-card {
  margin-top: 2px;
}

.element-card__title {
  font-weight: 600;
}

.element-card__sub {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.empty-tip {
  padding: 14px 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.6;
}

.blocker-head {
  font-weight: 600;
  margin-bottom: 4px;
}

.blocker-list {
  margin: 0;
  padding-left: 18px;
}

.sub-title {
  font-weight: 600;
  font-size: 13px;
  margin-top: 4px;
}

.warn-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.warn-list__item {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.notes {
  border-top: 1px dashed var(--td-component-stroke);
  padding-top: 8px;
}

.notes__title {
  font-weight: 600;
  font-size: 12px;
  margin-bottom: 4px;
}

.notes__item {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  line-height: 1.6;
}
</style>
