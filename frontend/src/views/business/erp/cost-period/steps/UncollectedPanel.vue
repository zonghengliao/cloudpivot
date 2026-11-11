<template>
  <div class="uncollected-panel" data-testid="cu-panel">
    <!-- ==================== 头部：一句话结论 + 刷新 ==================== -->
    <div class="panel-head">
      <div class="panel-head__main">
        <span class="panel-head__title">未归集清单</span>
        <t-tag
          size="small"
          :theme="res && res.gap_count > 0 ? 'warning' : 'success'"
          variant="light"
          data-testid="cu-summary"
        >
          {{ summary }}
        </t-tag>
        <span v-if="res?.batch" class="panel-head__meta" data-testid="cu-batch">
          批次 {{ res.batch.batch_no }}（{{ res.batch.status }}）
        </span>
        <span v-else-if="res" class="panel-head__meta" data-testid="cu-batch">
          本期还没有成本批次（点「计算」会自动建）
        </span>
      </div>
      <div class="panel-head__ops">
        <t-button
          size="small"
          variant="outline"
          :loading="loading"
          data-testid="cu-btn-refresh"
          @click="reload"
        >
          刷新
        </t-button>
        <t-button
          v-if="showRecalc"
          size="small"
          theme="primary"
          data-testid="cu-btn-recalc"
          @click="emit('recalc')"
        >
          去计算
        </t-button>
      </div>
    </div>

    <t-alert
      v-if="error"
      theme="error"
      :message="error"
      class="panel-alert"
      data-testid="cu-error"
    >
      <template #operation>
        <t-button size="small" variant="outline" @click="reload">重试</t-button>
      </template>
    </t-alert>

    <t-alert
      v-if="!loading && !error && !periodCode"
      theme="warning"
      message="先选期间：缺口清单是按「会计期间」自查的"
      class="panel-alert"
      data-testid="cu-no-period"
    />

    <!-- ==================== ① 未挂成本对象的领料 ==================== -->
    <t-card :bordered="false" class="block" data-testid="cu-block-stock-out">
      <div class="block__head">
        <span class="block__title">{{ stockOut.title }}</span>
        <t-tag
          size="small"
          :theme="gapKindTheme(stockOut.kind)"
          variant="light"
          data-testid="cu-count-stock-out"
        >
          {{ stockOut.count }} 张
        </t-tag>
        <span class="block__amount">金额合计 <b class="money">{{ formatMoney(stockOut.amount_total) }}</b></span>
        <span class="block__spacer" />
        <t-link
          v-if="stockOut.jump"
          theme="primary"
          data-testid="cu-jump-stock-out"
          @click="openJump(stockOut.jump)"
        >
          去领料单列表
        </t-link>
      </div>
      <p v-if="stockOut.count > 0" class="block__hint" data-testid="cu-hint-stock-out">
        {{ stockOut.hint }}
      </p>
      <t-table
        v-if="stockOut.rows.length > 0"
        row-key="stock_out_id"
        size="small"
        :data="stockOut.rows"
        :columns="stockOutColumns"
        hover
        data-testid="cu-table-stock-out"
      >
        <template #doc_no="{ row }">
          <t-link theme="primary" class="mono" @click.stop="openJump(row.jump)">{{ row.doc_no }}</t-link>
        </template>
        <template #qty_total="{ row }"><span class="num">{{ row.qty_total }}</span></template>
        <template #amount_total="{ row }">
          <span class="money">{{ formatMoney(row.amount_total) }}</span>
        </template>
        <template #doc_date="{ row }"><span class="mono">{{ row.doc_date }}</span></template>
      </t-table>
      <p v-else class="block__empty" data-testid="cu-empty-stock-out">
        {{ blockFooterText(stockOut) }}
      </p>
      <p v-if="stockOut.truncated" class="block__foot" data-testid="cu-truncated-stock-out">
        {{ blockFooterText(stockOut) }}
      </p>
    </t-card>

    <!-- ==================== ② 缺工时的在产对象 ==================== -->
    <t-card :bordered="false" class="block" data-testid="cu-block-labor-missing">
      <div class="block__head">
        <span class="block__title">{{ laborMissing.title }}</span>
        <t-tag
          size="small"
          :theme="gapKindTheme(laborMissing.kind)"
          variant="light"
          data-testid="cu-count-labor-missing"
        >
          {{ laborMissing.count }} 个
        </t-tag>
        <span class="block__amount">在制金额 <b class="money">{{ formatMoney(laborMissing.amount_total) }}</b></span>
        <span class="block__spacer" />
        <t-link
          v-if="laborMissing.jump"
          theme="primary"
          data-testid="cu-jump-labor-missing"
          @click="openJump(laborMissing.jump)"
        >
          去工时台账
        </t-link>
      </div>
      <p v-if="laborMissing.count > 0" class="block__hint" data-testid="cu-hint-labor-missing">
        {{ laborMissing.hint }}
      </p>
      <t-table
        v-if="laborMissing.rows.length > 0"
        row-key="cost_object_id"
        size="small"
        :data="laborMissing.rows"
        :columns="laborMissingColumns"
        hover
        data-testid="cu-table-labor-missing"
      >
        <template #object_no="{ row }">
          <t-link theme="primary" class="mono" @click.stop="openJump(row.jump)">{{ row.object_no }}</t-link>
        </template>
        <template #material_amount="{ row }">
          <span class="money">{{ formatMoney(row.material_amount) }}</span>
        </template>
        <template #draft_labor_rows="{ row }">
          <t-tag v-if="row.draft_labor_rows > 0" size="small" theme="warning" variant="light">
            {{ row.draft_labor_rows }} 行待确认
          </t-tag>
          <span v-else class="muted">未录</span>
        </template>
        <template #hint="{ row }"><span class="hint-cell">{{ row.hint }}</span></template>
      </t-table>
      <p v-else class="block__empty" data-testid="cu-empty-labor-missing">
        {{ blockFooterText(laborMissing) }}
      </p>
      <p v-if="laborMissing.truncated" class="block__foot">{{ blockFooterText(laborMissing) }}</p>
    </t-card>

    <!-- ==================== ③ 缺费率的工时行 ==================== -->
    <t-card :bordered="false" class="block" data-testid="cu-block-labor-rate">
      <div class="block__head">
        <span class="block__title">{{ laborRate.title }}</span>
        <t-tag
          size="small"
          :theme="gapKindTheme(laborRate.kind)"
          variant="light"
          data-testid="cu-count-labor-rate"
        >
          {{ laborRate.count }} 行
        </t-tag>
        <span class="block__amount">受影响工时 <b class="num">{{ laborRate.hours_total }}</b> 小时</span>
        <span class="block__spacer" />
        <t-link
          v-if="laborRate.jump"
          theme="primary"
          data-testid="cu-jump-labor-rate"
          @click="openJump(laborRate.jump)"
        >
          去工时台账
        </t-link>
      </div>
      <p v-if="laborRate.count > 0" class="block__hint" data-testid="cu-hint-labor-rate">
        {{ laborRate.hint }}
      </p>
      <t-table
        v-if="laborRate.rows.length > 0"
        row-key="labor_id"
        size="small"
        :data="laborRate.rows"
        :columns="laborRateColumns"
        hover
        data-testid="cu-table-labor-rate"
      >
        <template #work_date="{ row }"><span class="mono">{{ row.work_date }}</span></template>
        <template #cost_object_no="{ row }"><span class="mono">{{ row.cost_object_no || row.cost_object_id }}</span></template>
        <template #employee_name="{ row }">{{ row.employee_name || '—' }}</template>
        <template #dept_name="{ row }">{{ row.dept_name || '—' }}</template>
        <template #hours="{ row }"><span class="num">{{ row.hours }}</span></template>
        <template #rate="{ row }"><span class="num warn">{{ row.rate }}</span></template>
        <template #jump="{ row }">
          <t-link theme="primary" @click.stop="openJump(row.jump)">去补费率</t-link>
        </template>
      </t-table>
      <p v-else class="block__empty" data-testid="cu-empty-labor-rate">
        {{ blockFooterText(laborRate) }}
      </p>
      <p v-if="laborRate.truncated" class="block__foot">{{ blockFooterText(laborRate) }}</p>
    </t-card>

    <!-- ==================== ④ 未摊完的费用池 ==================== -->
    <t-card :bordered="false" class="block" data-testid="cu-block-pool">
      <div class="block__head">
        <span class="block__title">{{ pool.title }}</span>
        <t-tag
          size="small"
          :theme="gapKindTheme(pool.kind)"
          variant="light"
          data-testid="cu-count-pool"
        >
          {{ pool.count }} 个池
        </t-tag>
        <span class="block__amount">未摊余额 <b class="money">{{ formatMoney(pool.amount_total) }}</b></span>
        <span class="block__spacer" />
      </div>
      <p v-if="pool.count > 0" class="block__hint" data-testid="cu-hint-pool">{{ pool.hint }}</p>
      <t-table
        v-if="pool.rows.length > 0"
        row-key="account_code"
        size="small"
        :data="pool.rows"
        :columns="poolColumns"
        hover
        data-testid="cu-table-pool"
      >
        <template #dept_name="{ row }">{{ row.dept_name }}</template>
        <template #account_label="{ row }"><span class="mono">{{ row.account_label }}</span></template>
        <template #pool_amount="{ row }"><span class="money">{{ formatMoney(row.pool_amount) }}</span></template>
        <template #allocated_amount="{ row }">
          <span class="money">{{ formatMoney(row.allocated_amount) }}</span>
        </template>
        <template #remaining_amount="{ row }">
          <span class="money warn">{{ formatMoney(row.remaining_amount) }}</span>
        </template>
        <template #reason="{ row }">
          <t-tag size="small" :theme="poolReasonTheme(row.reason)" variant="light">{{ row.reason_label }}</t-tag>
        </template>
        <template #jump="{ row }">
          <t-link v-if="row.jump" theme="primary" @click.stop="openJump(row.jump)">去建规则</t-link>
          <span v-else class="muted">点「计算」</span>
        </template>
      </t-table>
      <p v-else class="block__empty" data-testid="cu-empty-pool">{{ blockFooterText(pool) }}</p>
      <p v-if="pool.truncated" class="block__foot">{{ blockFooterText(pool) }}</p>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 「未归集清单」面板（COST-B2-12 / ARCH-9 §6.2.2 ② 「自查缺口」）。
 *
 * 成本核算向导第 ② 步用的**页内组件**（设计 §6.3：缺口清单属页面私有的三个组件之一）。
 * 只吃一个入参 `period-code`，四块缺口全部由后端 `/cost-period/uncollected?period_code=`
 * 一次给齐 —— 前端不做任何口径判断（分母、可点击 id、怎么修文案全在后端）。
 *
 * # 为什么每块都是独立表格
 *
 * 四块的「一行」长得完全不同（一张出库单 / 一个生产批 / 一行工时 / 一个费用池），
 * 强行抽成通用表格会把列定义、跳转文案、单位（张 / 个 / 行 / 个池）都变成 if-else，
 * 反而更难维护。后端给的四块结构稳定（`kind` + `rows[]`），这里按块写死列即可。
 *
 * # `jump` 的处理
 *
 * 后端拼好前端路由串（含 `/business/erp/...` 前缀与 query），本组件只 `router.push`。
 * `jump === null` ⇒ 动作在**本页**（池缺口「未摊完」= 点「计算」），渲染成文字提示，
 * 不制造死链接。
 */
import { computed, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';

import {
  fetchUncollectedByPeriod,
  blockFooterText,
  gapKindTheme,
  gapKindTitle,
  poolReasonTheme,
  rowJump,
  summaryText,
  type CostUncollectedRes,
  type GapBlock,
  type LaborMissingGapRow,
  type LaborRateGapRow,
  type PoolGapRow,
  type StockOutGapRow,
} from '@/api/business/erp/cost-uncollected';
import { errorText } from '@/views/business/erp/shared/error-text';
import { formatMoney } from '@/views/business/erp/shared/money';

const props = withDefaults(
  defineProps<{
    /** 会计期间 `yyyy-MM`（空串 = 还没选期间，面板只提示不请求） */
    periodCode: string;
    /** 是否显示「去计算」按钮（向导第 ② 步给 true） */
    showRecalc?: boolean;
  }>(),
  { periodCode: '', showRecalc: false },
);

const emit = defineEmits<{ recalc: [] }>();

const router = useRouter();
const loading = ref(false);
const error = ref('');
const res = ref<CostUncollectedRes | null>(null);

/** 空块兜底（还没请求 / 请求失败时模板不判空） */
function emptyBlock<T>(kind: string): GapBlock<T> {
  return {
    kind,
    title: gapKindTitle(kind),
    count: 0,
    amount_total: '0',
    hours_total: '0',
    hint: '',
    jump: null,
    truncated: false,
    rows: [],
  };
}

const stockOut = computed<GapBlock<StockOutGapRow>>(
  () => res.value?.stock_out_without_object ?? emptyBlock('STOCK_OUT_WITHOUT_OBJECT'),
);
const laborMissing = computed<GapBlock<LaborMissingGapRow>>(
  () => res.value?.labor_missing ?? emptyBlock('LABOR_MISSING'),
);
const laborRate = computed<GapBlock<LaborRateGapRow>>(
  () => res.value?.labor_rate_missing ?? emptyBlock('LABOR_RATE_MISSING'),
);
const pool = computed<GapBlock<PoolGapRow>>(
  () => res.value?.pool_unallocated ?? emptyBlock('POOL_UNALLOCATED'),
);

const summary = computed(() =>
  res.value ? summaryText(res.value) : props.periodCode ? '正在自查…' : '选好期间后这里会列出缺口',
);

// ==================== 列定义 ====================

const stockOutColumns: PrimaryTableCol[] = [
  { colKey: 'doc_no', title: '出库单号', width: 190 },
  { colKey: 'doc_date', title: '出库日期', width: 108 },
  { colKey: 'warehouse_name', title: '仓库', width: 120, ellipsis: true },
  { colKey: 'item_count', title: '明细行', width: 80, align: 'right' },
  { colKey: 'qty_total', title: '数量', width: 90, align: 'right' },
  { colKey: 'amount_total', title: '金额', width: 120, align: 'right' },
];

const laborMissingColumns: PrimaryTableCol[] = [
  { colKey: 'object_no', title: '成本对象', width: 200 },
  { colKey: 'draft_labor_rows', title: '草稿工时', width: 120 },
  { colKey: 'material_amount', title: '本期材料投入', width: 130, align: 'right' },
  { colKey: 'hint', title: '怎么修', ellipsis: true },
];

const laborRateColumns: PrimaryTableCol[] = [
  { colKey: 'work_date', title: '日期', width: 104 },
  { colKey: 'cost_object_no', title: '成本对象', width: 190, ellipsis: true },
  { colKey: 'employee_name', title: '员工', width: 80 },
  { colKey: 'dept_name', title: '工作中心', width: 110, ellipsis: true },
  { colKey: 'hours', title: '工时', width: 80, align: 'right' },
  { colKey: 'rate', title: '工资率', width: 90, align: 'right' },
  { colKey: 'jump', title: '操作', width: 100 },
];

const poolColumns: PrimaryTableCol[] = [
  { colKey: 'dept_name', title: '车间', width: 120, ellipsis: true },
  { colKey: 'account_label', title: '费用科目', width: 200, ellipsis: true },
  { colKey: 'pool_amount', title: '池总额', width: 120, align: 'right' },
  { colKey: 'allocated_amount', title: '已分摊', width: 120, align: 'right' },
  { colKey: 'remaining_amount', title: '未摊余额', width: 120, align: 'right' },
  { colKey: 'reason', title: '原因', width: 110 },
  { colKey: 'jump', title: '操作', width: 100 },
];

// ==================== 动作 ====================

/** 跳转（后端拼好的前端路由；空串不跳，避免把用户送到空白页） */
function openJump(jump: string | null | undefined): void {
  const target = rowJump({ jump });
  if (!target) {
    MessagePlugin.info('这个缺口要在本页处理：请点「计算」重算后重看');
    return;
  }
  void router.push(target);
}

/** 拉一次缺口（`period-code` 变化由父组件负责调） */
async function reload(): Promise<void> {
  const period = props.periodCode.trim();
  if (!period) {
    res.value = null;
    error.value = '';
    return;
  }
  loading.value = true;
  error.value = '';
  try {
    res.value = await fetchUncollectedByPeriod(period);
  } catch (err) {
    error.value = errorText(err, '加载未归集清单失败');
    res.value = null;
  } finally {
    loading.value = false;
  }
}

defineExpose({ reload });

// 期间一变就自查（`immediate`：父组件一挂载、期间已带值时不白等一次用户操作）
watch(
  () => props.periodCode,
  () => {
    void reload();
  },
  { immediate: true },
);
</script>

<style scoped>
.uncollected-panel {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.panel-head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.panel-head__main {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.panel-head__title {
  font-size: 15px;
  font-weight: 600;
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-secondary, #666);
}

.panel-head__ops {
  margin-left: auto;
  display: flex;
  gap: 8px;
}

.panel-alert {
  margin-bottom: 4px;
}

.block {
  border-radius: 8px;
  background: var(--td-bg-color-container, #fff);
}

.block__head {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin-bottom: 8px;
}

.block__title {
  font-size: 14px;
  font-weight: 600;
}

.block__amount {
  font-size: 12px;
  color: var(--td-text-color-secondary, #666);
}

.block__spacer {
  margin-left: auto;
}

.block__hint {
  margin: 0 0 8px;
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-text-color-secondary, #666);
}

.block__empty {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder, #999);
}

.block__foot {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--td-warning-color, #ed7b2f);
}

.hint-cell {
  font-size: 12px;
  color: var(--td-text-color-secondary, #666);
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.num {
  font-variant-numeric: tabular-nums;
}

.money {
  font-variant-numeric: tabular-nums;
}

.warn {
  color: var(--td-warning-color, #ed7b2f);
}

.muted {
  color: var(--td-text-color-placeholder, #999);
  font-size: 12px;
}
</style>
