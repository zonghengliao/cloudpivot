<!--
  「成本计算单」面板（COST-B3-11 / ARCH-9 §6.2.2 ③「计算结果表」）。

  向导第 ③ 步的页内组件（设计 §6.3：页面私有的三个组件之一）。

  # 一张表说清「钱怎么来的」

  列 = 对象 × 料工费 × 约当 × 单位成本 × 完工/在制（设计 §6.2.2 ③ 原文）。
  行**可展开**：展开后是这张单的完整明细（期初在制 / 完工 / 期末在制 / 完工进度 /
  两个约当产量 / 本期三项投入 / 三项单位成本 / 完工入库单价）—— 卡面验收的
  「试算结果可看明细」就是这一块。

  # 表尾是「按明细现算」的合计

  单头 `total_*` 是**结算时落库**的审计口径，`DRAFT` 阶段恒 0（后端 DTO 头部写死了这条
  口径），所以表尾合计一律用 `summaryOf(items)` 现算，不用 `detail.total_*`。

  # 没有计算单时不许假装算过

  设计 §5.2 #27 `POST /cost-period/calc`（计算编排）尚未落地，没有 HTTP 入口能在页面上
  「跑一次计算」。所以空表时给的是**明确的现状说明 + 建批次按钮**，不是「试算完成，0 个对象」
   —— 后者会让用户以为算完了、去结算，然后被 6 条断言拦下。
-->
<template>
  <div class="calc-sheet" data-testid="cp-calc-sheet">
    <!-- ==================== 头部：一句话结论 + 守恒自证 ==================== -->
    <div class="sheet-head">
      <span class="sheet-head__summary" data-testid="cp-calc-summary">{{ summaryText }}</span>
      <span class="sheet-head__spacer" />
      <t-tag
        v-if="items.length > 0"
        size="small"
        :theme="allConserved ? 'success' : 'danger'"
        variant="light"
        data-testid="cp-calc-conserved"
      >
        {{ allConserved ? '逐行成本守恒 ✓' : '有行不守恒（见红行）' }}
      </t-tag>
      <t-tooltip v-if="showCalcNote" content="计算口径在后端（料→工→费→约当→完工/在制），本页只展示结果">
        <t-icon name="help-circle" class="sheet-head__help" />
      </t-tooltip>
    </div>

    <t-alert v-if="notice" theme="info" class="sheet-alert" data-testid="cp-calc-notice">
      {{ notice }}
    </t-alert>

    <t-table
      v-if="items.length > 0"
      row-key="id"
      size="small"
      :data="items"
      :columns="columns"
      :loading="loading"
      hover
      :expanded-row-keys="expandedKeys"
      data-testid="cp-calc-table"
      @expand-change="handleExpand"
    >
      <template #object_no="{ row }">
        <div class="obj-cell">
          <span class="mono">{{ row.object_no || shortId(row.cost_object_id) }}</span>
          <span v-if="row.product_name" class="obj-cell__name">{{ row.product_name }}</span>
        </div>
      </template>
      <template #completed_qty="{ row }"><span class="num">{{ qtyText(row.completed_qty) }}</span></template>
      <template #wip_end_qty="{ row }"><span class="num">{{ qtyText(row.wip_end_qty) }}</span></template>
      <template #equiv_conversion="{ row }">
        <span class="num">{{ qtyText(row.equiv_conversion) }}</span>
      </template>
      <template #mat_cur="{ row }"><span class="money">{{ formatMoney(row.mat_cur) }}</span></template>
      <template #labor_cur="{ row }"><span class="money">{{ formatMoney(row.labor_cur) }}</span></template>
      <template #oh_cur="{ row }"><span class="money">{{ formatMoney(row.oh_cur) }}</span></template>
      <template #finished_cost="{ row }"><span class="money">{{ formatMoney(row.finished_cost) }}</span></template>
      <template #wip_end_cost="{ row }">
        <span class="money" :class="{ 'is-bad': !itemConserved(row) }">{{ formatMoney(row.wip_end_cost) }}</span>
      </template>
      <template #status="{ row }">
        <t-tag size="small" :theme="row.status === 'SETTLED' ? 'success' : 'default'" variant="light">
          {{ itemStatusLabel(row.status) }}
        </t-tag>
      </template>

      <!-- 展开：这张单的完整明细 -->
      <template #expandedRow="{ row }">
        <div class="detail" data-testid="cp-calc-detail">
          <div v-for="group in detailGroups" :key="group" class="detail__group">
            <div class="detail__title">{{ group }}</div>
            <div class="detail__grid">
              <div
                v-for="line in rowsOfGroup(row, group)"
                :key="line.label"
                class="detail__item"
              >
                <span class="detail__label">{{ line.label }}</span>
                <span class="detail__value">{{ line.value }}</span>
              </div>
            </div>
          </div>
          <p class="detail__foot" :class="{ 'is-bad': !itemConserved(row) }">
            成本守恒：完工转出 {{ formatMoney(row.finished_cost) }} + 期末在制
            {{ formatMoney(row.wip_end_cost) }} = 期初在制 {{ formatMoney(row.wip_beg_cost) }} + 本期投入
            {{ formatMoney(sumOf(row)) }} —— {{ itemConserved(row) ? '平' : '不平，请重算或联系管理员' }}
          </p>
          <p v-if="row.stock_in_id" class="detail__foot" data-testid="cp-calc-stock-in">
            生产入库单：<span class="mono">{{ shortId(row.stock_in_id) }}</span>
          </p>
        </div>
      </template>
    </t-table>

    <!-- 空表：现状说明（不是「算完了」） -->
    <t-alert
      v-else-if="!loading"
      theme="warning"
      class="sheet-alert"
      data-testid="cp-calc-empty"
    >
      <template #message>
        {{ emptyText }}
      </template>
      <template #operation>
        <t-button
          v-if="showCreate"
          size="small"
          theme="primary"
          :loading="creating"
          data-testid="cp-btn-create-batch"
          @click="emit('create')"
        >
          建批次
        </t-button>
      </template>
    </t-alert>

    <!-- 合计（有行才显示；口径 = 按明细现算） -->
    <div v-if="items.length > 0" class="sheet-total" data-testid="cp-calc-total">
      <span class="sheet-total__label">合计（{{ summary.object_count }} 个对象）</span>
      <span class="sheet-total__item">料 <b class="money">{{ formatMoney(summary.total_material) }}</b></span>
      <span class="sheet-total__item">工 <b class="money">{{ formatMoney(summary.total_labor) }}</b></span>
      <span class="sheet-total__item">费 <b class="money">{{ formatMoney(summary.total_overhead) }}</b></span>
      <span class="sheet-total__item">投入 <b class="money">{{ formatMoney(summary.total_cost) }}</b></span>
      <span class="sheet-total__item">完工转出 <b class="money">{{ formatMoney(summary.total_finished) }}</b></span>
      <span class="sheet-total__item">期末在制 <b class="money">{{ formatMoney(summary.total_wip) }}</b></span>
      <span class="sheet-total__spacer" />
      <span class="sheet-total__check" :class="{ 'is-bad': !totalsConserved }">
        投入 = 完工转出 + 期末在制 {{ totalsConserved ? '✓' : '✗' }}
      </span>
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * 成本计算单面板（步③）。
 *
 * 只吃数据、不自己取数：`items` / `summary` 由页面从 `GET /cost-period/{id}` 拿，
 * 这样「建批次 → 重取详情 → 表格刷新」是同一条数据流，不出现两处各拉一次的情况。
 */
import { computed, ref } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';

import {
  COST_ITEM_STATUS_LABELS,
  type CostPeriodSummary,
  type CostSheetItem,
} from '@/api/business/erp/cost-period';
import { formatMoney, shortId } from '@/views/business/erp/shared/money';
import {
  itemConserved,
  itemDetailRows,
  qtyText,
  summaryOf,
  type DetailRow,
} from '../cost-period-logic';

const props = withDefaults(
  defineProps<{
    /** 计算单行（空 = 还没有结果） */
    items: CostSheetItem[];
    /** 后端现算合计（不传就按 `items` 自己算） */
    summary?: CostPeriodSummary | null;
    /** 加载中 */
    loading?: boolean;
    /** 是否显示「建批次」（期间还没批次时给 true） */
    showCreate?: boolean;
    /** 建批次中 */
    creating?: boolean;
    /** 头部补充说明（如「计算入口尚未上线」） */
    notice?: string;
    /** 空表文案 */
    emptyText?: string;
  }>(),
  {
    summary: null,
    loading: false,
    showCreate: false,
    creating: false,
    notice: '',
    emptyText:
      '本期还没有成本计算单。计算结果由后端的「计算」流程写入（设计 §5.2 #27 的计算编排入口尚未上线），' +
      '所以这里先只能看已有结果；先建批次、把料工费录齐，等入口上线后一键重算。',
  },
);

const emit = defineEmits<{ create: [] }>();

/** 合计：优先用后端现算值，缺就本地按明细算（两者口径一致） */
const summary = computed<CostPeriodSummary>(() => props.summary ?? summaryOf(props.items));

/** 头部一句话 */
const summaryText = computed<string>(() => {
  if (props.loading) {
    return '正在读取成本计算单…';
  }
  if (props.items.length === 0) {
    return '本期还没有成本计算单';
  }
  return (
    `${summary.value.object_count} 个成本对象 · 投入 ${formatMoney(summary.value.total_cost)}` +
    ` · 完工转出 ${formatMoney(summary.value.total_finished)}` +
    ` · 期末在制 ${formatMoney(summary.value.total_wip)}`
  );
});

/** 逐行守恒（库层 `ck_biz_cost_item_cons` 已硬拦，这里是界面自证） */
const allConserved = computed<boolean>(() => props.items.every((item) => itemConserved(item)));

/** 合计守恒：投入 = 完工转出 + 期末在制 */
const totalsConserved = computed<boolean>(() => {
  const sum = summary.value;
  const left = Number(sum.total_cost);
  const right = Number(sum.total_finished) + Number(sum.total_wip);
  return Math.abs(left - right) < 0.00005;
});

/** 头部帮助图标只在有数据时有意义 */
const showCalcNote = computed<boolean>(() => props.items.length > 0);

/** 展开行的键（默认收起，用户点开才看明细） */
const expandedKeys = ref<string[]>([]);

function handleExpand(keys: Array<string | number>): void {
  expandedKeys.value = keys.map((key) => String(key));
}

/** 明细分组顺序（与 `itemDetailRows` 的 group 值一致） */
const detailGroups = ['数量', '金额', '单位成本'] as const;

function rowsOfGroup(row: CostSheetItem, group: string): DetailRow[] {
  return itemDetailRows(row).filter((line) => line.group === group);
}

/** 单行「本期投入」合计（明细页脚用） */
function sumOf(row: CostSheetItem): string {
  const total = Number(row.mat_cur) + Number(row.labor_cur) + Number(row.oh_cur);
  return Number.isFinite(total) ? total.toFixed(2) : '0.00';
}

function itemStatusLabel(status: string): string {
  return COST_ITEM_STATUS_LABELS[status] ?? status;
}

/**
 * 列（按 1440 视口校过：内容区约 1128px，抽屉 1000px − padding ≈ 992px）。
 *
 * 列宽合计 ≈ 986px：对象 150 / 完工 74 / 在制 74 / 约当 84 / 料 100 / 工 100 / 费 100 /
 * 完工单价 96 / 完工转出 104 / 期末在制 104 —— 展开列 40（TDesign 自带）。
 */
const columns: PrimaryTableCol[] = [
  { colKey: 'row-expand', width: 40 },
  { colKey: 'object_no', title: '成本对象', width: 140, ellipsis: true },
  { colKey: 'completed_qty', title: '完工数量', width: 70, align: 'right' },
  { colKey: 'wip_end_qty', title: '在制数量', width: 70, align: 'right' },
  { colKey: 'equiv_conversion', title: '约当产量', width: 78, align: 'right' },
  { colKey: 'mat_cur', title: '材料', width: 92, align: 'right' },
  { colKey: 'labor_cur', title: '人工', width: 92, align: 'right' },
  { colKey: 'oh_cur', title: '制造费用', width: 92, align: 'right' },
  { colKey: 'finished_cost', title: '完工转出', width: 100, align: 'right' },
  { colKey: 'wip_end_cost', title: '期末在制', width: 100, align: 'right' },
  { colKey: 'status', title: '状态', width: 76 },
];
</script>

<style scoped>
.calc-sheet {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.sheet-head {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.sheet-head__summary {
  color: var(--td-text-color-primary);
  font-weight: 500;
}

.sheet-head__spacer {
  flex: 1;
}

.sheet-head__help {
  color: var(--td-text-color-placeholder);
  cursor: help;
}

.sheet-alert {
  margin: 0;
}

.obj-cell {
  display: flex;
  flex-direction: column;
  line-height: 1.3;
}

.obj-cell__name {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
  font-size: 12px;
}

.num,
.money {
  font-variant-numeric: tabular-nums;
}

.is-bad {
  color: var(--td-error-color);
  font-weight: 600;
}

.detail {
  padding: 10px 12px 12px 40px;
  background: var(--td-bg-color-container-hover);
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.detail__group {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.detail__title {
  font-size: 12px;
  font-weight: 600;
  color: var(--td-text-color-secondary);
}

.detail__grid {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 4px 16px;
}

.detail__item {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  font-size: 12px;
}

.detail__label {
  color: var(--td-text-color-secondary);
}

.detail__value {
  font-variant-numeric: tabular-nums;
  color: var(--td-text-color-primary);
}

.detail__foot {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.sheet-total {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 8px 12px;
  font-size: 13px;
  background: var(--td-bg-color-secondarycontainer);
  border-radius: 4px;
}

.sheet-total__label {
  font-weight: 600;
}

.sheet-total__spacer {
  flex: 1;
}

.sheet-total__check {
  color: var(--td-success-color);
  font-weight: 600;
}

.sheet-total__check.is-bad {
  color: var(--td-error-color);
}
</style>
