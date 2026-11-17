/**
 * 「预测 vs 实际」对比视图（M3-23 前端页面）纯逻辑单测。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * # 为什么测 `.ts` 不测 `.vue`
 *
 * 「偏差率 `null` 该显示 `—` 还是 `0.00%`」「图取前几行、按什么顺序」「金额字符串怎么转数字」
 * 全在 `forecast-variance-logic.ts` 里，`.vue` 只渲染。这些一旦漂移 `vue-tsc` 抓不到
 * （编译期两边都是 `string | null`），而线上表现是**看错数**：预测为 0 的行被画成「正好命中」、
 * 前 12 行按名字重排后与表格对不上。
 *
 * # 源码即契约（直接读后端文件断言，前后端改名当场红）
 *
 * 1. 粒度值 / 标签与 `sales_target_dto.rs` 的 `FORECAST_DIM_*` + `dimension_label` 逐字一致；
 * 2. 偏差档位四档值与标签与 `DEVIATION_*` + `deviation_label` 一致；
 * 3. 路由 `GET /business/erp/sales-forecast/variance` 与权限码 `business:erp:sales-forecast:list`
 *    在后端 handler 里真实存在，且前端 API 用的就是这条路径；
 * 4. `ForecastVarianceRow` / `ForecastVarianceSummary` / `ForecastVarianceRes` 的字段集合
 *    与后端 DTO **双向齐全**（后端加字段前端没跟上、或前端抄错名字都会红）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

import type {
  ForecastVarianceRow,
  ForecastVarianceSummary,
} from '../../src/api/business/erp/sales-target.ts';
import {
  CHART_NAME_LIMIT,
  CHART_ROW_LIMIT,
  DIMENSION_OPTIONS,
  axisMoneyText,
  buildVarianceChartOption,
  deviationMeta,
  dimensionHint,
  moneyOf,
  pickChartRows,
  qtyText,
  rateText,
  shortName,
  summaryHeadline,
  toNumber,
  toleranceText,
  varianceTone,
  worstText,
} from '../../src/views/business/erp/sales-target/forecast-variance-logic.ts';

// ==================== 夹具 ====================

/** 对比明细一行（只写被测字段，其余给默认值） */
function row(patch: Partial<ForecastVarianceRow> & { display_name: string }): ForecastVarianceRow {
  return {
    dimension: 'CUSTOMER',
    period_code: '2026-11',
    forecast_id: 'f1',
    forecast_no: 'SFC-20261101-000001',
    version_no: 1,
    confidence: 'MEDIUM',
    customer_id: 'c1',
    customer_name: patch.display_name,
    material_id: null,
    material_name: null,
    has_forecast: true,
    forecast_amount: '100.00',
    forecast_qty: '10.0000',
    actual_amount: '100.00',
    actual_qty: '10.0000',
    variance_amount: '0.00',
    variance_rate: '0.00',
    variance_qty: '0.0000',
    variance_qty_rate: '0.00',
    deviation_level: 'ON_TARGET',
    ...patch,
  };
}

/** 汇总结构（只写被测字段） */
function summary(patch: Partial<ForecastVarianceSummary> = {}): ForecastVarianceSummary {
  return {
    row_count: 0,
    forecast_count: 0,
    unforecast_count: 0,
    over_count: 0,
    under_count: 0,
    on_target_count: 0,
    forecast_amount: '0.00',
    actual_amount: '0.00',
    variance_amount: '0.00',
    variance_rate: null,
    forecast_qty: '0.0000',
    actual_qty: '0.0000',
    variance_qty: '0.0000',
    variance_qty_rate: null,
    mape_pct: null,
    worst: null,
    ...patch,
  };
}

// ==================== 数字转换 ====================

test('toNumber：Decimal 字符串转数字，空值 / 非法 → null（不是 0）', () => {
  assert.equal(toNumber('11300.00'), 11300);
  assert.equal(toNumber('0.00'), 0);
  assert.equal(toNumber(-1234.5), -1234.5);
  assert.equal(toNumber(''), null);
  assert.equal(toNumber(null), null);
  assert.equal(toNumber(undefined), null);
  assert.equal(toNumber('abc'), null, '非法字符串不能悄悄变 0');
  assert.equal(toNumber('NaN'), null);
});

test('moneyOf：缺失按 0（漏预测行的预测金额确实是 0），非法也按 0 不抛', () => {
  assert.equal(moneyOf(null), 0);
  assert.equal(moneyOf(undefined), 0);
  assert.equal(moneyOf('0.00'), 0);
  assert.equal(moneyOf('16,000.00'), 0, '带千分位的不是合法数字 —— 宁可 0 也不 NaN');
  assert.equal(moneyOf('12345.67'), 12345.67);
});

// ==================== 名字截断 ====================

test('shortName：短名原样，超长补 …，空 → （未命名）', () => {
  assert.equal(shortName('北京华信'), '北京华信');
  assert.equal(shortName('  北京华信  '), '北京华信', '首尾空白要修掉');
  assert.equal(shortName(null), '（未命名）');
  assert.equal(shortName(''), '（未命名）');
  assert.equal(shortName('   '), '（未命名）');
  const long = '北京华信科技股份有限公司天津分公司';
  const cut = shortName(long);
  assert.equal(Array.from(cut).length, CHART_NAME_LIMIT + 1, '截断后 = max 个字 + 省略号');
  assert.ok(cut.endsWith('…'));
  assert.ok(long.startsWith(cut.slice(0, -1)), '截断点不能漂');
  // 正好等于上限：不补省略号
  const exact = Array.from(long).slice(0, CHART_NAME_LIMIT).join('');
  assert.equal(shortName(exact), exact);
});

test('shortName：按码点截断，不劈开 emoji（否则横轴显示半个字符）', () => {
  const name = '📈📉📊📋📌🔖🧾💼🧮📦🚚🏷️🎯'; // 13 个码点形态的名字
  const cut = shortName(name, 4);
  assert.equal(cut, '📈📉📊📋…');
  assert.equal(Array.from(cut).length, 5);
});

// ==================== 取行 ====================

test('pickChartRows：只切前 N 行，顺序完全照后端（|偏差| 倒序）', () => {
  const rows = [
    row({ display_name: 'A', variance_amount: '900.00' }),
    row({ display_name: 'B', variance_amount: '-800.00' }),
    row({ display_name: 'C', variance_amount: '700.00' }),
  ];
  assert.deepEqual(
    pickChartRows(rows, 2).map((item) => item.display_name),
    ['A', 'B'],
    '不许在前端重排（重排后图与表对不上）',
  );
  assert.equal(pickChartRows(rows).length, 3, '默认上限内全画');
  assert.deepEqual(pickChartRows(rows, 0), []);
  assert.deepEqual(pickChartRows(null), []);
  assert.deepEqual(pickChartRows([]), []);
});

test('pickChartRows：默认上限 = CHART_ROW_LIMIT，超出按顺序截前 12 行', () => {
  const rows = Array.from({ length: CHART_ROW_LIMIT + 5 }, (_, index) =>
    row({ display_name: `客户${index}`, variance_amount: `${1000 - index}.00` }),
  );
  const picked = pickChartRows(rows);
  assert.equal(picked.length, CHART_ROW_LIMIT);
  assert.equal(picked[0]?.display_name, '客户0');
  assert.equal(picked[CHART_ROW_LIMIT - 1]?.display_name, `客户${CHART_ROW_LIMIT - 1}`);
});

// ==================== 图表 option ====================

interface SeriesLike {
  name?: string;
  type?: string;
  data?: unknown[];
  yAxisIndex?: number;
  connectNulls?: boolean;
  valueFormatter?: (value: unknown) => string;
}

/** 从 option 里取系列（测试用，避免 any） */
function seriesOf(option: unknown): SeriesLike[] {
  return ((option as { series?: SeriesLike[] }).series ?? []) as SeriesLike[];
}

test('buildVarianceChartOption：三序列（预测柱 / 实际柱 / 偏差率线），名字与图例一致', () => {
  const option = buildVarianceChartOption({
    rows: [
      row({ display_name: '北京华信', forecast_amount: '100000.00', actual_amount: '90000.00' }),
      row({ display_name: '上海联通', forecast_amount: '50000.00', actual_amount: '60000.00' }),
    ],
    dimension_label: '按客户',
  });
  const series = seriesOf(option);
  assert.equal(series.length, 3);
  assert.deepEqual(
    series.map((item) => item.name),
    ['预测金额', '实际金额', '偏差率'],
  );
  assert.deepEqual(
    series.map((item) => item.type),
    ['bar', 'bar', 'line'],
  );
  assert.equal(series[2]?.yAxisIndex, 1, '偏差率走右侧轴');
  assert.equal(series[2]?.connectNulls, false, 'null 率必须断线，不能连成 0');
  const legend = (option as { legend?: { data?: string[] } }).legend;
  assert.deepEqual(legend?.data, ['预测金额', '实际金额', '偏差率']);
});

test('buildVarianceChartOption：金额按数字画柱，偏差率 null 保持 null（不补 0）', () => {
  const option = buildVarianceChartOption({
    rows: [
      row({ display_name: 'A', forecast_amount: '100000.00', actual_amount: '120000.00', variance_rate: '20.00' }),
      row({
        display_name: 'B',
        has_forecast: false,
        forecast_amount: '0.00',
        actual_amount: '30000.00',
        variance_rate: null,
        deviation_level: 'NO_FORECAST',
      }),
    ],
  });
  const series = seriesOf(option);
  assert.deepEqual(series[0]?.data, [100000, 0], '漏预测行预测额 0');
  assert.deepEqual(series[1]?.data, [120000, 30000]);
  assert.deepEqual(series[2]?.data, [20, null], '预测为 0 的行偏差率是 null，不是 0');
  // 金额 tooltip 走千分位；比率 tooltip 走百分比，null 显示破折号
  assert.equal(series[0]?.valueFormatter?.(100000), '100,000.00');
  assert.equal(series[2]?.valueFormatter?.(null), '—');
  assert.equal(series[2]?.valueFormatter?.(20), '20.00%');
});

test('buildVarianceChartOption：横轴用截断名，行多时轴标签旋转', () => {
  const longName = '北京华信科技股份有限公司天津分公司';
  const two = buildVarianceChartOption({ rows: [row({ display_name: longName }), row({ display_name: 'B' })] });
  const xAxis = (two as { xAxis?: { data?: string[]; axisLabel?: { rotate?: number } } }).xAxis;
  assert.equal(xAxis?.data?.[0], shortName(longName));
  assert.equal(xAxis?.axisLabel?.rotate, 0, '≤6 行不旋转');

  const many = buildVarianceChartOption({
    rows: Array.from({ length: 8 }, (_, index) => row({ display_name: `客户${index}` })),
  });
  const manyAxis = (many as { xAxis?: { axisLabel?: { rotate?: number } } }).xAxis;
  assert.equal(manyAxis?.axisLabel?.rotate, 30, '>6 行旋转 30 度，否则标签互相压');
});

test('buildVarianceChartOption：零行不抛，data 全空（页面据此显示空状态）', () => {
  const option = buildVarianceChartOption({ rows: [] });
  const series = seriesOf(option);
  assert.equal(series.length, 3);
  for (const item of series) {
    assert.deepEqual(item.data, []);
  }
  assert.deepEqual((option as { xAxis?: { data?: unknown[] } }).xAxis?.data, []);
});

// ==================== 档位 / 文本 ====================

test('deviationMeta：四档文案与颜色，未知档位不静默当命中', () => {
  assert.deepEqual(deviationMeta('OVER'), { label: '超出预测', theme: 'danger' });
  assert.deepEqual(deviationMeta('UNDER'), { label: '低于预测', theme: 'warning' });
  assert.deepEqual(deviationMeta('ON_TARGET'), { label: '基本命中', theme: 'success' });
  assert.deepEqual(deviationMeta('NO_FORECAST'), { label: '漏预测', theme: 'default' });
  assert.deepEqual(deviationMeta('WHAT'), { label: '未知档位', theme: 'default' });
});

test('rateText：null / 空 → —（不是 0.00%），数字两位小数', () => {
  assert.equal(rateText(null), '—');
  assert.equal(rateText(undefined), '—');
  assert.equal(rateText(''), '—');
  assert.equal(rateText(0), '0.00%');
  assert.equal(rateText('85.5'), '85.50%');
  assert.equal(rateText('-28.5714'), '-28.57%');
});

test('varianceTone：正 = 实际超出，负 = 实际低于，0 = 持平', () => {
  assert.equal(varianceTone('120.00'), 'over');
  assert.equal(varianceTone(-0.01), 'under');
  assert.equal(varianceTone('0.00'), 'flat');
  assert.equal(varianceTone(null), 'flat');
});

test('axisMoneyText：≥1 万走「万」，小数 1 位；1 万以下千分位取整', () => {
  assert.equal(axisMoneyText(0), '0');
  assert.equal(axisMoneyText(9999), '9,999');
  assert.equal(axisMoneyText(10000), '1.0万');
  assert.equal(axisMoneyText(123456), '12.3万');
  assert.equal(axisMoneyText(-20000), '-2.0万');
  assert.equal(axisMoneyText(Number.NaN), '');
});

test('toleranceText：去掉尾零（5.00 → 5），缺失回落后端缺省 5', () => {
  assert.equal(toleranceText('5.00'), '5');
  assert.equal(toleranceText('2.50'), '2.5');
  assert.equal(toleranceText(0), '0');
  assert.equal(toleranceText('100'), '100');
  assert.equal(toleranceText(null), '5');
});

test('qtyText：4 位小数去尾零；0 照实显示（= 只预测金额），null → —', () => {
  assert.equal(qtyText('10.0000'), '10');
  assert.equal(qtyText('10.5000'), '10.5');
  assert.equal(qtyText('0.0000'), '0');
  assert.equal(qtyText(null), '—');
});

// ==================== 汇总排版 ====================

test('summaryHeadline：行数 / 档位计数 / MAPE 一次说清，MAPE 空也照实', () => {
  const text = summaryHeadline(
    summary({
      row_count: 5,
      forecast_count: 4,
      unforecast_count: 1,
      over_count: 1,
      under_count: 1,
      on_target_count: 2,
      mape_pct: '48.33',
    }),
  );
  assert.ok(text.includes('共 5 行（4 有预测 / 1 漏预测）'), text);
  assert.ok(text.includes('命中 2 · 超出 1 · 低于 1'), text);
  assert.ok(text.includes('MAPE 48.33%'), text);

  const noMape = summaryHeadline(summary({ row_count: 1, unforecast_count: 1 }));
  assert.ok(noMape.includes('MAPE —'), noMape);
});

test('worstText：漏预测行不带「偏差率」（null 不是比率），有率时带上', () => {
  assert.equal(worstText(null), null);
  assert.equal(
    worstText({
      display_name: '北京华信',
      forecast_no: 'SFC-20261101-000002',
      variance_amount: '-30000.00',
      variance_rate: '-28.57',
      deviation_level: 'UNDER',
    }),
    '北京华信（低于预测）偏差 -30,000.00，偏差率 -28.57%',
  );
  assert.equal(
    worstText({
      display_name: '上海联通',
      forecast_no: null,
      variance_amount: '30000.00',
      variance_rate: null,
      deviation_level: 'NO_FORECAST',
    }),
    '上海联通（漏预测）偏差 30,000.00',
  );
});

test('dimensionHint：只提其它粒度且条数 > 0；当前粒度不重复提；没有就空串', () => {
  const counts = [
    { dimension: 'CUSTOMER' as const, dimension_label: '按客户', row_count: 3 },
    { dimension: 'MATERIAL' as const, dimension_label: '按物料', row_count: 2 },
    { dimension: 'CUSTOMER_MATERIAL' as const, dimension_label: '按客户 × 物料', row_count: 0 },
    { dimension: 'ALL' as const, dimension_label: '全公司', row_count: 1 },
  ];
  const hint = dimensionHint(counts, 'CUSTOMER');
  assert.ok(hint.includes('按物料 2 条'), hint);
  assert.ok(hint.includes('全公司 1 条'), hint);
  assert.ok(!hint.includes('按客户 3 条'), '当前粒度不该出现在「其它粒度」里');
  assert.ok(!hint.includes('按客户 × 物料'), '0 条的粒度不提');
  assert.equal(dimensionHint(counts, 'ALL'), '本期间其它粒度还有预测：按客户 3 条、按物料 2 条 —— 换粒度看才能对上数（预测按粒度隔离，实际合计同源）');
  assert.equal(dimensionHint([], 'CUSTOMER'), '');
  assert.equal(dimensionHint(null, 'CUSTOMER'), '');
});

// ==================== 源码即契约 ====================

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, '..', '..', '..');
const DTO_PATH = resolve(REPO_ROOT, 'backend/app/src/business/erp/sales/sales_target_dto.rs');
const HANDLER_PATH = resolve(REPO_ROOT, 'backend/app/src/business/erp/sales/sales_target_handler.rs');
const API_PATH = resolve(REPO_ROOT, 'frontend/src/api/business/erp/sales-target.ts');

/** 读文件（缺文件就让测试红，不静默跳过） */
function read(path: string): string {
  return readFileSync(path, 'utf8');
}

/** 取 `pub struct X { ... }` 里的字段名（`pub name: T,`） */
function structFields(source: string, structName: string): string[] {
  const start = source.indexOf(`pub struct ${structName} {`);
  assert.ok(start >= 0, `后端 DTO 里找不到 struct ${structName}`);
  const body = source.slice(start, source.indexOf('\n}', start));
  return [...body.matchAll(/pub (\w+):/g)].map((match) => match[1] ?? '');
}

/** 取 TS 接口的字段名（`name:` / `name?:`） */
function interfaceFields(source: string, interfaceName: string): string[] {
  const start = source.indexOf(`export interface ${interfaceName} {`);
  assert.ok(start >= 0, `前端 API 里找不到 interface ${interfaceName}`);
  const body = source.slice(start, source.indexOf('\n}', start));
  return [...body.matchAll(/^\s{2}(\w+)\??:/gm)].map((match) => match[1] ?? '');
}

test('契约①：四个粒度值 / 标签与后端 FORECAST_DIM_* + dimension_label 逐字一致', () => {
  const dto = read(DTO_PATH);
  const pairs: [string, string, string][] = [
    ['FORECAST_DIM_CUSTOMER', 'CUSTOMER', '按客户'],
    ['FORECAST_DIM_MATERIAL', 'MATERIAL', '按物料'],
    ['FORECAST_DIM_CUSTOMER_MATERIAL', 'CUSTOMER_MATERIAL', '按客户 × 物料'],
    ['FORECAST_DIM_ALL', 'ALL', '全公司'],
  ];
  assert.deepEqual(
    DIMENSION_OPTIONS.map((item) => item.value),
    pairs.map(([, value]) => value),
    '前端下拉顺序必须与 FORECAST_DIMENSIONS 一致（缺省 CUSTOMER 在第一位）',
  );
  for (const [constant, value, label] of pairs) {
    assert.ok(dto.includes(`pub const ${constant}: &str = "${value}";`), `后端常量 ${constant} = "${value}" 变了`);
    assert.ok(dto.includes(`${constant} => "${label}"`), `后端 dimension_label 的「${label}」变了`);
    assert.equal(DIMENSION_OPTIONS.find((item) => item.value === value)?.label, label);
  }
});

test('契约②：四档偏差值与 label 与后端 DEVIATION_* + deviation_label 逐字一致', () => {
  const dto = read(DTO_PATH);
  const pairs: [string, string, string, string][] = [
    ['DEVIATION_OVER', 'OVER', '超出预测', 'danger'],
    ['DEVIATION_UNDER', 'UNDER', '低于预测', 'warning'],
    ['DEVIATION_ON_TARGET', 'ON_TARGET', '基本命中', 'success'],
    ['DEVIATION_NO_FORECAST', 'NO_FORECAST', '漏预测', 'default'],
  ];
  for (const [constant, value, label, theme] of pairs) {
    assert.ok(dto.includes(`pub const ${constant}: &str = "${value}";`), `后端常量 ${constant} 变了`);
    assert.ok(dto.includes(`${constant} => "${label}"`), `后端 deviation_label 的「${label}」变了`);
    const meta = deviationMeta(value);
    assert.equal(meta.label, label);
    assert.equal(meta.theme, theme);
  }
});

test('契约③：路由与权限码 —— 后端 GET /variance（sales-forecast:list），前端调的就是这条', () => {
  const handler = read(HANDLER_PATH);
  assert.ok(
    handler.includes('path = "/business/erp/sales-forecast/variance"'),
    '后端对比路由改名了 —— 前端 fetchForecastVariance 的路径要跟着改',
  );
  assert.ok(handler.includes('#[nik::get("/business/erp/sales-forecast/variance")]'), '对比接口必须是 GET');
  assert.ok(
    handler.includes('permission = "business:erp:sales-forecast:list"'),
    '对比接口复用预测列表权限码（M3-21 决定，零新增码）',
  );
  const api = read(API_PATH);
  assert.ok(api.includes('`${BASE}/sales-forecast/variance`'), '前端路径要拼 `${BASE}/sales-forecast/variance`');
  assert.ok(api.includes('get<ForecastVarianceRes>(`${BASE}/sales-forecast/variance`'), '必须走 GET');
  assert.ok(
    api.includes("const BASE = '/api/business/erp';"),
    'BASE 必须带 `/api` 前缀（漏了会命中 SPA fallback 返回 HTML）',
  );
});

test('契约④：对比行 / 汇总 / 出参字段与后端 DTO 双向齐全（改名当场红）', () => {
  const dto = read(DTO_PATH);
  const api = read(API_PATH);
  const cases: [string, string][] = [
    ['ForecastVarianceRow', 'ForecastVarianceRow'],
    ['ForecastVarianceSummary', 'ForecastVarianceSummary'],
    ['ForecastVarianceRes', 'ForecastVarianceRes'],
    ['ForecastVarianceWorst', 'ForecastVarianceWorst'],
    ['ForecastDimensionCount', 'ForecastDimensionCount'],
  ];
  for (const [rustName, tsName] of cases) {
    const rust = structFields(dto, rustName).sort();
    const ts = interfaceFields(api, tsName).sort();
    assert.ok(rust.length > 0, `${rustName} 没解析到字段`);
    assert.deepEqual(ts, rust, `${tsName} 字段与后端 ${rustName} 不一致（缺字段 / 多字段 / 抄错名）`);
  }
});

test('契约⑤：后端 query 只三个入参（period_code / dimension / tolerance_pct），前端不多发', () => {
  const dto = read(DTO_PATH);
  const queryFields = structFields(dto, 'ForecastVarianceQuery');
  assert.deepEqual(queryFields.sort(), ['dimension', 'period_code', 'tolerance_pct']);
  const api = read(API_PATH);
  const tsQuery = interfaceFields(api, 'ForecastVarianceQuery').filter((name) => name !== '');
  assert.deepEqual(tsQuery.sort(), ['dimension', 'period_code', 'tolerance_pct']);
});
