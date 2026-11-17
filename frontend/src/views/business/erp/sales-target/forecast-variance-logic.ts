/**
 * 「预测 vs 实际」对比视图的**纯逻辑**（M3-23 前端页面）。
 *
 * `.vue` 只负责取数与渲染，凡是「算错会让人看错数」的判断都放这里，由
 * `tests/unit/erp-sales-forecast-variance.test.ts` 逐条钉住：
 *
 * | 函数 | 钉住什么 | 算错会怎样 |
 * |------|----------|------------|
 * | `toNumber` / `moneyOf` | `Decimal` 字符串转数字，`null` 不当 0 | 预测金额 `null` 画成 0 万的柱子 |
 * | `shortName` | 长名字截断（按**码点**，不劈开 emoji） | 客户名撑爆横轴，图看不清 |
 * | `pickChartRows` | 只取前 N 行，**保持后端顺序**（|偏差| 倒序） | 前端重排 ⇒ 图和表对不上 |
 * | `buildVarianceChartOption` | 三序列（预测柱 / 实际柱 / 偏差率线）、`null` 率保持空档 | `null` 画成 0% ⇒ 「预测为 0 的偏差」看不出异常 |
 * | `deviationMeta` | 四档文案与颜色 | 档位认错 ⇒ 「超出」显示成「低于」 |
 * | `rateText` | `null` ⇒ `—`（不是 `0.00%`） | 预测为 0 的行报 0%，看着像没偏差 |
 * | `summaryHeadline` | 汇总一句话（行数/档位/MAPE） | 自己另算一遍 ⇒ 与后端 summary 口径漂移 |
 * | `axisMoneyText` | 金额轴「万」级缩写 | 8 位数把轴挤成省略号 |
 *
 * **一句话口径**（后端 `summarize_variance` 同源，前端不重算）：
 * 偏差金额 = 实际 − 预测（正 = 实际超出预测）；合计偏差率 = Σ实际 ÷ Σ预测，
 * 而 MAPE 是**行偏差率的平均**——两者不等，别互相顶替（M3-22 单测钉过）。
 */
import type { EChartsCoreOption } from 'echarts';
import type {
  DecimalText,
  DeviationLevel,
  ForecastDimension,
  ForecastDimensionCount,
  ForecastVarianceRow,
  ForecastVarianceSummary,
  ForecastVarianceWorst,
} from '@/api/business/erp/sales-target';

/**
 * 对比粒度下拉（顺序与后端 `FORECAST_DIMENSIONS` 逐字一致：客户 → 物料 → 客户×物料 → 全公司）。
 *
 * ⚠️ 定义在**逻辑文件**而不是 `api/sales-target.ts`：后者运行时 `import '@/api/request'`
 * 会碰 `import.meta.env`，`tsx --test` 下拿不到（单测加载即炸）。常量放这里，单测能直接吃。
 */
export const DIMENSION_OPTIONS: { value: ForecastDimension; label: string }[] = [
  { value: 'CUSTOMER', label: '按客户' },
  { value: 'MATERIAL', label: '按物料' },
  { value: 'CUSTOMER_MATERIAL', label: '按客户 × 物料' },
  { value: 'ALL', label: '全公司' },
];

/** 图里最多画多少行（后端按 |偏差| 倒序，前 N 行就是偏差最大的 N 行） */
export const CHART_ROW_LIMIT = 12;

/** 横轴标签最多几个字（按码点计，超了补 `…`） */
export const CHART_NAME_LIMIT = 12;

/**
 * `Decimal` 字符串 / 数字 → number；拿不到有效数字时返回 `null`（**不是 0**）。
 *
 * 为什么要 `null`：偏差率 `null` 表示「预测金额为 0，比率无意义」，画成 0 会被
 * 误读成「正好命中」；金额侧则用 `moneyOf` 兜 0（漏预测行的预测金额确实是 0）。
 */
export function toNumber(value: DecimalText | null | undefined): number | null {
  if (value === null || value === undefined || value === '') {
    return null;
  }
  const num = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(num) ? num : null;
}

/** 金额 → number（缺失按 0；画柱子用） */
export function moneyOf(value: DecimalText | null | undefined): number {
  return toNumber(value) ?? 0;
}

/** 显示名截断：`null` / 空白 ⇒ `全公司`；超长补 `…`（按码点，不劈开 emoji） */
export function shortName(name: string | null | undefined, max: number = CHART_NAME_LIMIT): string {
  const text = (name ?? '').trim();
  if (text === '') {
    return '（未命名）';
  }
  const chars = Array.from(text);
  if (chars.length <= max) {
    return text;
  }
  return `${chars.slice(0, Math.max(1, max)).join('')}…`;
}

/**
 * 取图上要画的前 N 行。
 *
 * **顺序完全保持后端给的**（按 |偏差金额| 倒序 → 实际额 → 名字），前端不重排：
 * 一重排，表格（全量）和图形（前 N）就对不上号。
 */
export function pickChartRows<T>(rows: readonly T[] | null | undefined, limit: number = CHART_ROW_LIMIT): T[] {
  if (!rows || rows.length === 0 || limit <= 0) {
    return [];
  }
  return rows.slice(0, limit);
}

/** 金额轴缩写：`≥1 万` 走「万」，否则两位内取整（0 显示 `0`） */
export function axisMoneyText(value: number): string {
  if (!Number.isFinite(value)) {
    return '';
  }
  const abs = Math.abs(value);
  if (abs >= 10000) {
    return `${(value / 10000).toFixed(1)}万`;
  }
  return value.toLocaleString('zh-CN', { maximumFractionDigits: 0 });
}

/**
 * 比率文本：`null` ⇒ `—`（**不写 0.00%**），数字 ⇒ 两位小数 + `%`。
 * 传进来的如果是非法字符串，原样回显（不吞错）。
 */
export function rateText(value: DecimalText | null | undefined): string {
  const num = toNumber(value);
  if (num === null) {
    return '—';
  }
  return `${num.toFixed(2)}%`;
}

/**
 * 金额文本：`12,000.00`；缺失 ⇒ `—`。
 *
 * 与 `api/sales-target.ts` 的 `formatMoney` 同口径，但**本地实现**：那个模块运行时
 * `import '@/api/request'` 会碰 `import.meta.env`，`tsx --test` 加载即炸；
 * 逻辑文件必须能脱离 axios 单独被单测吃下去。
 */
export function moneyText(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const num = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(num)) {
    return String(value);
  }
  return num.toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** 偏差方向：`over` 实际超出预测 / `under` 实际低于预测 / `flat` 持平 */
export function varianceTone(value: DecimalText | null | undefined): 'over' | 'under' | 'flat' {
  const num = moneyOf(value);
  if (num > 0) {
    return 'over';
  }
  return num < 0 ? 'under' : 'flat';
}

/** 偏差档位的文案与颜色（四档与后端 `deviation_label` 逐字一致） */
export function deviationMeta(level: DeviationLevel | string): {
  label: string;
  theme: 'danger' | 'warning' | 'success' | 'default';
} {
  switch (level) {
    case 'OVER':
      return { label: '超出预测', theme: 'danger' };
    case 'UNDER':
      return { label: '低于预测', theme: 'warning' };
    case 'ON_TARGET':
      return { label: '基本命中', theme: 'success' };
    case 'NO_FORECAST':
      return { label: '漏预测', theme: 'default' };
    default:
      return { label: '未知档位', theme: 'default' };
  }
}

/**
 * 图表 option（ECharts）。**不写 `color`**：配色由 `EChart.vue` 的配色引擎按当前主题注入。
 *
 * 三条序列：预测金额（柱）、实际金额（柱）、偏差率（右侧轴折线）。
 * - `null` 率保留为 `null`（折线断开），不补 0；
 * - 金额柱子带 `valueFormatter`，tooltip 里直接是 `12,000.00` 而不是 `12000`。
 */
export function buildVarianceChartOption(
  res: { rows: readonly ForecastVarianceRow[]; dimension_label?: string },
  limit: number = CHART_ROW_LIMIT,
): EChartsCoreOption {
  const rows = pickChartRows(res.rows, limit);
  const names = rows.map((row) => shortName(row.display_name));
  const forecast = rows.map((row) => moneyOf(row.forecast_amount));
  const actual = rows.map((row) => moneyOf(row.actual_amount));
  const rates = rows.map((row) => toNumber(row.variance_rate));
  return {
    grid: { left: 12, right: 16, top: 44, bottom: 8, containLabel: true },
    tooltip: { trigger: 'axis', axisPointer: { type: 'shadow' } },
    legend: { top: 4, data: ['预测金额', '实际金额', '偏差率'] },
    xAxis: {
      type: 'category',
      data: names,
      axisLabel: { interval: 0, rotate: rows.length > 6 ? 30 : 0 },
    },
    yAxis: [
      {
        type: 'value',
        name: '金额',
        axisLabel: { formatter: axisMoneyText },
      },
      {
        type: 'value',
        name: '偏差率%',
        axisLabel: { formatter: (value: number) => `${value}%` },
        splitLine: { show: false },
      },
    ],
    series: [
      {
        name: '预测金额',
        type: 'bar',
        barMaxWidth: 28,
        data: forecast,
        valueFormatter: (value: unknown) => moneyText(value as DecimalText),
      },
      {
        name: '实际金额',
        type: 'bar',
        barMaxWidth: 28,
        data: actual,
        valueFormatter: (value: unknown) => moneyText(value as DecimalText),
      },
      {
        name: '偏差率',
        type: 'line',
        yAxisIndex: 1,
        data: rates,
        connectNulls: false,
        symbolSize: 6,
        valueFormatter: (value: unknown) => rateText(value as DecimalText | null),
      },
    ],
  };
}

/** 汇总一句话：行数 / 档位计数 / MAPE（口径与后端 `summary` 一致，前端只排版） */
export function summaryHeadline(summary: ForecastVarianceSummary): string {
  const parts = [
    `共 ${summary.row_count} 行（${summary.forecast_count} 有预测 / ${summary.unforecast_count} 漏预测）`,
    `命中 ${summary.on_target_count} · 超出 ${summary.over_count} · 低于 ${summary.under_count}`,
    `MAPE ${rateText(summary.mape_pct)}`,
  ];
  return parts.join('；');
}

/** 最贵偏差一行的人话（没有行 ⇒ `null`） */
export function worstText(worst: ForecastVarianceWorst | null | undefined): string | null {
  if (!worst) {
    return null;
  }
  const level = deviationMeta(worst.deviation_level).label;
  const rate = worst.variance_rate === null || worst.variance_rate === undefined ? '' : `，偏差率 ${rateText(worst.variance_rate)}`;
  return `${worst.display_name}（${level}）偏差 ${moneyText(worst.variance_amount)}${rate}`;
}

/**
 * 「换个粒度看」提示：本次粒度之外**当前版本预测条数 > 0** 的其它粒度。
 * 一条都没有 ⇒ 空串（不显示提示，避免噪音）。
 */
export function dimensionHint(
  counts: readonly ForecastDimensionCount[] | null | undefined,
  current: ForecastDimension | string,
): string {
  const others = (counts ?? []).filter((item) => item.dimension !== current && item.row_count > 0);
  if (others.length === 0) {
    return '';
  }
  const text = others.map((item) => `${item.dimension_label} ${item.row_count} 条`).join('、');
  return `本期间其它粒度还有预测：${text} —— 换粒度看才能对上数（预测按粒度隔离，实际合计同源）`;
}

/** 容差文本：`5.00` → `5`，`2.50` → `2.5`（后端 Decimal 回显去掉尾零，别显示成 ±5.00%） */
export function toleranceText(value: DecimalText | null | undefined): string {
  const num = toNumber(value);
  if (num === null) {
    return '5';
  }
  return String(Number(num.toFixed(2)));
}

/**
 * 数量文本：最多 4 位小数（与库侧 `NUMERIC(18,4)` 一致），去掉无意义的尾零；
 * `0` 也照实显示（预测数量 0 = 只预测金额，不是「没填」）。
 */
export function qtyText(value: DecimalText | null | undefined): string {
  const num = toNumber(value);
  if (num === null) {
    return '—';
  }
  return String(Number(num.toFixed(4)));
}
