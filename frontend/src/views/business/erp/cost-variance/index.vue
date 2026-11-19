<!--
  差异分析页（M5-19【M5·成本】差异分析：前端页）。

  一页讲清三件事：**三类差异（料 / 工 / 费）→ 整表合计 → 逐物料明细**。

  # 数据从哪来

  只消费 M5-11 的一条只读报表 `GET /api/business/erp/report/cost-variance`：
  一个物料一行 = 左标准（期间末生效 + 已发布的标准成本卡）右实际（当期活跃批次的成本计算单），
  出「标准应发生额 / 实际完工成本 / 差异额 / 差异率 / 方向 / 三要素拆分」+ 整表合计。

  **前端不重算任何公式**（连合计也不加总）：报表是服务端分页的，前端手里只有当前页，
  拿当前页加出来的「合计」翻页就变，而财务会拿它跟凭证对。后端 `totals` 是**按筛选后全集**聚合的。

  # 四屏（自上而下）

  | 区块 | 内容 | testid |
  |---|---|---|
  | 筛选 | 期间（**必填**）/ 物料关键字 / 状态四档 + 查询 / 重置 | `cv-filter-*` |
  | 汇总 | 6 张指标卡（行数 / 标准 / 实际 / 差异额 / 差异率 / 不可比部分）+ 取数口径 | `cv-stat-*`、`cv-basis` |
  | 三类差异 | 材料 / 人工 / 制造费用 三行合计表（差在哪一眼看得出） | `cv-elements` |
  | 明细 | 逐物料表（服务端分页），行可展开看两侧事实与单位口径 | `cv-table` |

  筛选条**不由 `ListPage` 渲染**（`:filters="[]"` + `:exportable="false"` ⇒ 组件的筛选卡整块不画），
  因为这张报表要「先看合计、再看明细」，合计卡必须落在筛选条与明细表之间；
  查询 / 重置直接驱动 `listRef.search()`，入参由 `cost-variance-logic::buildQuery` 统一拼（有单测）。

  # 三处「别当 bug 改」的口径

  1. **期间必填**：后端不默认「上个月」（猜错月份没人看得出）。期间为空时页面**不发请求**，
     直接画引导语（`PERIOD_REQUIRED_HINT`）。
  2. **`cost_ready = false` 不是「差异为 0」**：本期没有成本核算批次时，后端仍列标准侧、
     全部标「缺实际」。页面用 warning 条说清原因并给「去成本核算」的入口，
     **不**把那些行画成「差异 0」。
  3. **差异率 `null` = 标准额为 0，不计算**：显示「—」而不是 `0.00%`
     （0% 会被读成「没有差异」）。列表列 tooltip 与合计卡副标题都写明这句。

  # 差异不入账（ARCH-9 D-15）

  报表只让差异被看见，不动存货计价、不出凭证。页面底部的口径说明由后端 `note` 原文转达
  （只去掉 markdown 的 `**` 强调记号，不改口径本身）。
-->
<template>
  <div class="cv-page">
    <!-- ==================== ① 筛选 ==================== -->
    <t-card :bordered="false" class="cv-tool-card">
      <div class="tool-row tool-row--dense">
        <t-date-picker
          v-model="filter.period_code"
          class="cv-period"
          mode="month"
          value-type="YYYY-MM"
          format="YYYY-MM"
          placeholder="会计期间（必选）"
          data-testid="cv-filter-period"
          @change="handleSearch"
        />
        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="物料编码 / 名称"
          clearable
          data-testid="cv-filter-keyword"
          @enter="handleSearch"
        />
        <t-select
          v-model="filter.state"
          class="cv-state"
          :options="stateOptions"
          placeholder="全部状态"
          data-testid="cv-filter-state"
          @change="handleSearch"
        />
        <t-button theme="primary" :disabled="!periodReady" data-testid="cv-btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="cv-btn-reset" @click="handleReset">重置</t-button>
        <span class="tool-row__spacer" />
        <span class="tool-row__hint">期间必填；差异 = 实际 − 标准（正数 = 不利）</span>
      </div>
    </t-card>

    <!-- ==================== ② 引导 / 口径 ==================== -->
    <!--
      期间为空时的引导条：**防守分支**，不是常规路径。
      期间控件是「月份选择器 + 默认本月 + 不可清空」，正常操作下 `period_code` 永远有值；
      这里留着是为了「控件被换掉 / 传进来一个非法值」时页面仍然说人话，而不是直接打 400。
    -->
    <t-alert
      v-if="!periodReady"
      theme="info"
      :message="PERIOD_REQUIRED_HINT"
      data-testid="cv-period-hint"
    />

    <t-alert v-else-if="scopeHint" theme="warning" :message="scopeHint" data-testid="cv-scope-hint">
      <template #operation>
        <t-button size="small" variant="outline" data-testid="cv-btn-to-cost-period" @click="goCostPeriod">
          去成本核算
        </t-button>
      </template>
    </t-alert>

    <!-- ==================== ③ 整表合计 ==================== -->
    <!-- 合计卡只在**有行**时画：0 行时一排 0.00 只是噪声，空态文案与引导条已经把「为什么」说清了 -->
    <t-card v-if="data && data.total > 0" :bordered="false" class="cv-summary-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">整表合计</span>
          <span class="panel-head__meta" data-testid="cv-scope-meta">
            {{ data.period_code }}（{{ data.period_start }} ~ {{ data.period_end }}）
            <template v-if="data.batch"> · 批次 {{ data.batch.batch_no }}</template>
            <template v-else> · 无成本核算批次</template>
          </span>
        </div>
      </div>

      <div class="stat-row" data-testid="cv-stats">
        <div
          v-for="card in summary"
          :key="card.key"
          class="stat-card"
          :data-testid="`cv-stat-${card.key}`"
        >
          <span class="stat-card__label">{{ card.label }}</span>
          <span class="stat-card__value" :class="summaryValueClass(card.tone)">{{ card.value }}</span>
          <span class="stat-card__sub">{{ card.sub }}</span>
        </div>
      </div>

      <p class="cv-basis" data-testid="cv-basis">{{ basisText }}</p>
    </t-card>

    <!-- ==================== ④ 三类差异（料 / 工 / 费） ==================== -->
    <t-card v-if="data && data.total > 0" :bordered="false" class="cv-elements-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">三类差异</span>
          <span class="panel-head__meta" data-testid="cv-elements-meta">
            材料 / 人工 / 制造费用 —— 与整表合计同源（按筛选后全集）
          </span>
        </div>
      </div>

      <t-table
        row-key="key"
        size="small"
        :data="elementRows"
        :columns="elementColumns"
        data-testid="cv-elements"
      >
        <template #label="{ row }">
          <span class="cv-element-name">{{ row.label }}</span>
        </template>
        <template #stdAmount="{ row }">
          <span class="mono">{{ row.stdAmount }}</span>
        </template>
        <template #actAmount="{ row }">
          <span class="mono">{{ row.actAmount }}</span>
        </template>
        <template #amount="{ row }">
          <span class="mono money" :class="toneClass(row.tone)">{{ row.amount }}</span>
        </template>
        <template #rate="{ row }">
          <span class="mono">{{ row.rate }}</span>
        </template>
        <template #direction="{ row }">
          <t-tag size="small" variant="light" :theme="row.tone">{{ row.directionLabel }}</t-tag>
        </template>
      </t-table>

      <p class="cv-note">
        差异 = 实际 − 标准，正数 = 不利（超支）、负数 = 有利（节约）；差异率 = 差异额 / 标准应发生额，
        标准额为 0 时不计算（显示「—」而不是 0%）。不可比的行（缺标准 / 缺实际）不在这张合计表里。
      </p>
    </t-card>

    <!-- ==================== ⑤ 逐物料明细（服务端分页） ==================== -->
    <ListPage
      ref="listRef"
      class="cv-list tool-row--dense"
      title="差异明细"
      testid="cv"
      table-testid="cv-table"
      meta-testid="cv-meta"
      btn-prefix="cv-btn"
      row-key="material_id"
      expandable
      :table-slots="['expandedRow']"
      :exportable="false"
      :columns="columns"
      :filters="[]"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
    >
      <template #meta-extra>
        <span class="cv-meta-suffix">{{ metaSuffix }}</span>
      </template>

      <!-- 物料：编码 + 名称，第二行规格 -->
      <template #material="{ row }">
        <div class="cv-material">
          <span class="cv-material__name">{{ rowMaterialText(row) }}</span>
          <span class="cv-material__spec">规格 {{ specText(row) }}</span>
        </div>
      </template>

      <!-- 状态：可比 / 缺标准 / 缺实际 -->
      <template #state="{ row }">
        <t-tag size="small" variant="light" :theme="rowStateTheme(row.state)">
          {{ rowStateText(row) }}
        </t-tag>
      </template>

      <template #qty="{ row }">
        <span class="mono">{{ completedQtyText(row) }}</span>
      </template>

      <template #std_amount="{ row }">
        <span class="mono">{{ row.variance ? moneyText(row.variance.std_amount) : '—' }}</span>
      </template>

      <template #act_amount="{ row }">
        <span class="mono">{{ row.variance ? moneyText(row.variance.act_amount) : '—' }}</span>
      </template>

      <!-- 差异额：不可比行不画 0（那是「没比」，不是「没差异」） -->
      <template #amount="{ row }">
        <span
          v-if="row.variance"
          class="mono money"
          :class="toneClass(directionTheme(row.variance.direction))"
        >
          {{ moneyText(row.variance.amount) }}
        </span>
        <span v-else class="cv-muted">—</span>
      </template>

      <template #rate="{ row }">
        <t-tooltip v-if="row.variance && !row.variance.rate" :content="RATE_UNDEFINED_HINT">
          <span class="mono cv-muted">—</span>
        </t-tooltip>
        <span v-else class="mono">{{ row.variance ? rateText(row.variance.rate) : '—' }}</span>
      </template>

      <template #direction="{ row }">
        <t-tag
          v-if="row.variance"
          size="small"
          variant="light"
          :theme="directionTheme(row.variance.direction)"
        >
          {{ row.variance.direction_label || directionLabel(row.variance.direction) }}
        </t-tag>
        <span v-else class="cv-muted">—</span>
      </template>

      <!-- 三类差异（行内紧凑）：料 600.00 · 工 -180.00 · 费 0.00 -->
      <template #elements="{ row }">
        <span class="mono cv-elements-text">{{ elementsCompactText(row) }}</span>
      </template>

      <template #hint="{ row }">
        <span class="cv-hint">{{ row.hint || '—' }}</span>
      </template>

      <!-- 空态：区分「没选期间」「筛选没命中」「这个月两边都没有」 -->
      <template #empty>
        <StateView
          state="empty"
          compact
          :title="emptyText"
          description="换一个期间 / 关键字，或先到「标准成本」与「成本核算」两页把数据准备好。"
          testid="cv-empty"
        />
      </template>

      <!-- 展开行：左标准 / 右实际 / 三要素 / 单位口径 / 该行怎么修 -->
      <template #expandedRow="{ row }">
        <div class="cv-detail" data-testid="cv-detail">
          <div class="cv-detail__cols">
            <div class="cv-detail__col" data-testid="cv-detail-standard">
              <div class="cv-detail__title">标准一侧（期间末生效 + 已发布）</div>
              <div v-if="standardFacts(row).length === 0" class="cv-muted">
                这一版标准还没发布或没建卡 —— 见下面「怎么修」
              </div>
              <dl v-else class="cv-facts">
                <template v-for="fact in standardFacts(row)" :key="`std-${fact.label}`">
                  <dt>{{ fact.label }}</dt>
                  <dd class="mono">{{ fact.value }}</dd>
                </template>
              </dl>
            </div>

            <div class="cv-detail__col" data-testid="cv-detail-actual">
              <div class="cv-detail__title">实际一侧（当期成本计算单）</div>
              <div v-if="actualFacts(row).length === 0" class="cv-muted">
                本期没有成本计算单（该期间未做成本核算，或这个物料未投产）
              </div>
              <dl v-else class="cv-facts">
                <template v-for="fact in actualFacts(row)" :key="`act-${fact.label}`">
                  <dt>{{ fact.label }}</dt>
                  <dd class="mono">{{ fact.value }}</dd>
                </template>
              </dl>
            </div>
          </div>

          <!-- 可比行：行内三要素 + 单位口径（数据来自同一条响应的 variance.elements，不多打请求） -->
          <template v-if="row.variance">
            <div class="cv-detail__title">这一行的三类差异</div>
            <t-table
              row-key="key"
              size="small"
              :data="rowElements(row)"
              :columns="elementColumns"
              data-testid="cv-detail-elements"
            >
              <template #label="{ row: element }">
                <span class="cv-element-name">{{ element.label }}</span>
              </template>
              <template #stdAmount="{ row: element }">
                <span class="mono">{{ element.stdAmount }}</span>
              </template>
              <template #actAmount="{ row: element }">
                <span class="mono">{{ element.actAmount }}</span>
              </template>
              <template #amount="{ row: element }">
                <span class="mono money" :class="toneClass(element.tone)">{{ element.amount }}</span>
              </template>
              <template #rate="{ row: element }">
                <span class="mono">{{ element.rate }}</span>
              </template>
              <template #direction="{ row: element }">
                <t-tag size="small" variant="light" :theme="element.tone">
                  {{ element.directionLabel }}
                </t-tag>
              </template>
            </t-table>

            <dl class="cv-facts cv-facts--inline">
              <template v-for="fact in varianceFacts(row.variance)" :key="`var-${fact.label}`">
                <dt>{{ fact.label }}</dt>
                <dd class="mono" :class="toneClass(fact.tone ?? 'default')">{{ fact.value }}</dd>
              </template>
            </dl>
          </template>

          <t-alert
            v-if="row.hint"
            theme="info"
            class="cv-detail__hint"
            :message="row.hint"
            data-testid="cv-detail-hint"
          />
        </div>
      </template>

      <!-- 页脚：口径说明（后端原文，只去 markdown 记号） -->
      <template #footer>
        <p v-if="noteText" class="cv-foot-note" data-testid="cv-note">{{ noteText }}</p>
      </template>
    </ListPage>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';

import {
  fetchCostVariance,
  type CostVarianceQuery,
  type CostVarianceRes,
  type CostVarianceRow,
} from '@/api/business/erp/cost-variance';
import {
  PAGE_SIZE,
  STATE_OPTIONS,
  directionLabel,
  directionTheme,
  rowStateTheme,
} from '@/api/business/erp/cost-variance-kinds';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, ListFetcher, ListQuery, ListQueryBuilder } from '@/components/ListPage/types';
import StateView from '@/components/StateView/index.vue';
import {
  PERIOD_REQUIRED_HINT,
  RATE_UNDEFINED_HINT,
  actualFacts,
  basisTextOf,
  buildQuery,
  completedQtyText,
  defaultPeriod,
  elementRows as toElementRows,
  elementsCompactText,
  emptyTextOf,
  metaSuffixOf,
  moneyText,
  noteText as stripNote,
  rateText,
  rowElements,
  rowMaterialText,
  rowStateText,
  scopeHintOf,
  specText,
  standardFacts,
  summaryCards,
  varianceFacts,
  type VarianceFilter,
} from './cost-variance-logic';

/** 状态筛选下拉（值 = 后端 `StateFilter` 字面值） */
const stateOptions = STATE_OPTIONS.map((option) => ({ label: option.label, value: option.value }));

const router = useRouter();

// ==================== 筛选 ====================

const filter = reactive<VarianceFilter>({
  period_code: defaultPeriod(),
  keyword: '',
  state: 'all',
});

/** 期间是否可用（不可用时不发请求，只画引导语） */
const periodReady = computed(() => buildQuery({ ...filter }, 1) !== null);

// ==================== 取数与汇总 ====================

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 最近一次成功的响应（汇总卡片 / 三类差异 / 口径 / 批次信息都从这里出） */
const data = ref<CostVarianceRes | null>(null);

const summary = computed(() => summaryCards(data.value));
const elementRows = computed(() => toElementRows(data.value?.totals.elements ?? []));
const basisText = computed(() => basisTextOf(data.value));
const scopeHint = computed(() => scopeHintOf(data.value));
const noteText = computed(() => stripNote(data.value?.note));
const emptyText = computed(() => emptyTextOf(data.value, { ...filter }));
const metaSuffix = computed(() => metaSuffixOf(data.value?.totals));

/**
 * 拉一页。
 *
 * 期间非法 ⇒ 返回空页且**不发请求**（`buildQuery` 已经判过，这里再判一次是因为
 * `ListPage` 首屏挂载就会调一次 fetcher，而用户可能在期间空着的时候刷新页面）。
 * 整份响应塞进 `data`：`list/total/page/page_size` 正好是 `PageData` 的形状，其余字段给汇总用。
 */
const fetcher: ListFetcher<CostVarianceRow> = async (query) => {
  const built = query as CostVarianceQuery;
  if (!built.period_code) {
    data.value = null;
    return { list: [], total: 0, page: 1, page_size: PAGE_SIZE };
  }
  data.value = null;
  const res = await fetchCostVariance(built);
  data.value = res;
  return res;
};

/** 入参拼装：唯一判据在 `cost-variance-logic::buildQuery`（有单测） */
const queryBuilder: ListQueryBuilder = (_state, page, pageSize): ListQuery =>
  buildQuery({ ...filter }, page, pageSize) ?? {};

// ==================== 交互 ====================

function handleSearch(): void {
  void listRef.value?.search();
}

function handleReset(): void {
  filter.keyword = '';
  filter.state = 'all';
  filter.period_code = defaultPeriod();
  handleSearch();
}

function goCostPeriod(): void {
  void router.push('/business/erp/cost-period');
}

// ==================== 列 ====================

const columns: ColumnDef[] = [
  { colKey: 'material', title: '物料', width: 200, ellipsis: true },
  { colKey: 'state', title: '状态', width: 84 },
  { colKey: 'qty', title: '完工数量', width: 90, align: 'right' },
  { colKey: 'std_amount', title: '标准应发生', width: 120, align: 'right' },
  { colKey: 'act_amount', title: '实际完工', width: 120, align: 'right' },
  { colKey: 'amount', title: '差异额', width: 120, align: 'right' },
  { colKey: 'rate', title: '差异率', width: 84, align: 'right' },
  { colKey: 'direction', title: '方向', width: 76 },
  { colKey: 'elements', title: '三类差异（料 / 工 / 费）', width: 210, ellipsis: true },
  { colKey: 'hint', title: '说明 / 怎么修', width: 220, ellipsis: true },
];

/** 三类差异表格列（整表合计与行内展开共用同一套列，两处口径不会漂） */
const elementColumns = [
  { colKey: 'label', title: '要素', width: 120 },
  { colKey: 'stdAmount', title: '标准应发生额', width: 160, align: 'right' as const },
  { colKey: 'actAmount', title: '实际完工成本', width: 160, align: 'right' as const },
  { colKey: 'amount', title: '差异额', width: 160, align: 'right' as const },
  { colKey: 'rate', title: '差异率', width: 110, align: 'right' as const },
  { colKey: 'direction', title: '方向', width: 90 },
];

// ==================== 展示小工具 ====================

/** 语义色 class（不利红 / 有利绿 / 其它无色） */
function toneClass(tone: 'default' | 'danger' | 'success'): string {
  if (tone === 'danger') {
    return 'money--bad';
  }
  if (tone === 'success') {
    return 'money--good';
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
</script>

<style scoped lang="less">
.cv-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.cv-tool-card,
.cv-summary-card,
.cv-elements-card {
  border-radius: 8px;
}

.cv-period {
  width: 160px;
}

.cv-state {
  width: 140px;
}

/* ==================== 汇总卡片 ==================== */

.stat-row {
  display: grid;
  grid-template-columns: repeat(6, minmax(0, 1fr));
  gap: 12px;
  margin-top: 8px;

  @media (max-width: 1600px) {
    grid-template-columns: repeat(3, minmax(0, 1fr));
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

.cv-basis {
  margin: 10px 0 0;
  padding: 8px 12px;
  border-left: 3px solid var(--td-brand-color);
  background: var(--td-bg-color-container-hover);
  border-radius: 4px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 20px;
}

/* ==================== 三类差异表 ==================== */

.cv-element-name {
  font-weight: 600;
}

.cv-note {
  margin: 10px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 20px;
}

/* ==================== 明细表 ==================== */

.cv-meta-suffix {
  color: var(--td-text-color-placeholder);
}

.cv-material {
  display: flex;
  flex-direction: column;
  line-height: 1.3;

  &__name {
    font-weight: 500;
  }

  &__spec {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }
}

.cv-elements-text {
  font-size: 12px;
}

.cv-hint {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.cv-muted {
  color: var(--td-text-color-placeholder);
}

.mono {
  font-variant-numeric: tabular-nums;
}

.money {
  font-weight: 600;
}

.money--bad {
  color: var(--td-error-color);
}

.money--good {
  color: var(--td-success-color);
}

/* ==================== 展开行 ==================== */

.cv-detail {
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

  &__hint {
    margin-top: 10px;
  }
}

.cv-facts {
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

  &--inline {
    margin-top: 8px;
    grid-template-columns: repeat(4, max-content minmax(0, 1fr));
  }
}

.cv-foot-note {
  margin: 8px 0 0;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 20px;
}
</style>
