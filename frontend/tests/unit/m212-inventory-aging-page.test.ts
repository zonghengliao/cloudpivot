/**
 * M2-12：库龄页**页面**契约单测（页面主体为新落地）。
 *
 * 运行：`cd frontend && pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 两层断言
 *
 * 1. **纯逻辑**（`aging-logic.ts`）：拼参（空筛选整键缺席、`include_consumed` 永远显式发、
 *    库龄区间写反要拦住）、数量 / 比例格式化（后端 `Decimal` 出的是字符串）、口径文案、
 *    行键与钻取。这些是「页面把库龄账看错 / 把筛选看丢」的入口。
 * 2. **源码即契约**（`index.vue` / `api/.../inventory-aging.ts` / `router/index.ts` /
 *    迁移 `V0427`）：真正会静默出错的是「模板里调了谁」—— 例如把合计改成客户端加总、
 *    页面行 auth_code 与取数码合并成一行、路由 path 与迁移 `sys_menu.path` 漂了（点菜单 404）。
 *    逻辑函数没变、逻辑单测全绿，页面却错了。
 *
 * 只读源码做子串 / 正则断言，不引 DOM、不引浏览器（与 `m207-transit-page.test.ts` 同款）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  AGING_PAGE_SIZE,
  AGING_TABS,
  PERM_AGING_LIST,
  PERM_AGING_PAGE,
  PERM_AGING_REPORT,
  PERM_AGING_SLOW_MOVING,
  SLOW_MOVING_CONFIG_KEY,
  SLOW_MOVING_DEFAULT_DAYS,
  ageDaysText,
  ageRangeOf,
  ageTheme,
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
  isLayerFiltered,
  layerEmptyText,
  materialText,
  reportEmptyText,
  reportTotalNote,
  requestErrorMessage,
  slowEmptyText,
  slowRatioTheme,
  thresholdHint,
  thresholdSourceText,
  truncatedNote,
  warehouseText,
} from '@/views/business/erp/inventory-aging/aging-logic';
import type { InventoryAgingReport, SlowMovingReport } from '@/api/business/erp/inventory-aging';

const ROOT = resolve(process.cwd());

function sourceOf(relative: string): string {
  return readFileSync(join(ROOT, relative), 'utf8');
}

/** 去掉注释再断言实现约束：文档里为了解释「为什么」会提到被禁止的写法本身 */
function withoutComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1');
}

/** 取 `<template>` 段：脚本里的同名函数不算页面渲染契约 */
function templateOf(source: string): string {
  const start = source.indexOf('<template>');
  const end = source.lastIndexOf('</template>');
  assert.ok(start >= 0 && end > start, '页面必须有 <template> 段');
  return source.slice(start + '<template>'.length, end);
}

const PAGE_PATH = 'src/views/business/erp/inventory-aging/index.vue';
const LOGIC_PATH = 'src/views/business/erp/inventory-aging/aging-logic.ts';
const API_PATH = 'src/api/business/erp/inventory-aging.ts';
const ROUTER_PATH = 'src/router/index.ts';
const MIG_UP_PATH = '../backend/app/migrations/business/V0427__erp_inventory_aging_page.up.sql';
const MIG_DOWN_PATH = '../backend/app/migrations/business/V0427__erp_inventory_aging_page.down.sql';

const PAGE = sourceOf(PAGE_PATH);
const TPL = templateOf(PAGE);
const LOGIC = sourceOf(LOGIC_PATH);
const API = sourceOf(API_PATH);
const ROUTER = sourceOf(ROUTER_PATH);
const MIG_UP = sourceOf(MIG_UP_PATH);
const MIG_DOWN = sourceOf(MIG_DOWN_PATH);

// ==================== ① 拼参：空筛选整键缺席 ====================

test('报表拼参：全空 → 空对象（后端取今天 + 全量）', () => {
  assert.deepEqual(buildReportQuery(emptyAgingFilter()), {});
});

test('报表拼参：空白字符串等于没填，不能发 ?as_of=', () => {
  const query = buildReportQuery({ as_of: '  ', material_id: '', warehouse_id: '   ' });
  assert.deepEqual(query, {});
  assert.equal('as_of' in query, false);
  assert.equal('material_id' in query, false);
  assert.equal('warehouse_id' in query, false);
});

test('报表拼参：填了就原样带上', () => {
  assert.deepEqual(buildReportQuery({ as_of: '2026-11-30', material_id: 'm-1', warehouse_id: 'w-1' }), {
    as_of: '2026-11-30',
    material_id: 'm-1',
    warehouse_id: 'w-1',
  });
});

test('呆滞拼参：days 留空整键缺席（让后端按系统参数 → 缺省判定）', () => {
  const query = buildSlowQuery(emptySlowFilter());
  assert.equal('days' in query, false, 'days 留空必须整键缺席：发 ?days= 会 400');
  assert.equal('as_of' in query, false);
});

test('呆滞拼参：days 显式填了就发数字（字符串要转 number）', () => {
  assert.equal(buildSlowQuery({ ...emptySlowFilter(), days: '30' }).days, 30);
  assert.equal(buildSlowQuery({ ...emptySlowFilter(), days: 180 }).days, 180);
});

test('批次层拼参：include_consumed 永远显式发（开关拨了必须生效）', () => {
  const off = buildLayerQuery(emptyLayerFilter(), 1, AGING_PAGE_SIZE);
  assert.equal(off.include_consumed, 'false');
  assert.equal(off.page, 1);
  assert.equal(off.page_size, AGING_PAGE_SIZE);
  assert.equal('min_age_days' in off, false);
  assert.equal('max_age_days' in off, false);
  assert.equal('lot_no' in off, false);

  const on = buildLayerQuery({ ...emptyLayerFilter(), include_consumed: true }, 2, 20);
  assert.equal(on.include_consumed, 'true');
  assert.equal(on.page, 2);
});

test('批次层拼参：库龄区间发数字，空白不发', () => {
  const query = buildLayerQuery({ ...emptyLayerFilter(), min_age_days: '31', max_age_days: 90 }, 1, 20);
  assert.equal(query.min_age_days, 31);
  assert.equal(query.max_age_days, 90);

  const blank = buildLayerQuery({ ...emptyLayerFilter(), min_age_days: '', max_age_days: '  ' }, 1, 20);
  assert.equal('min_age_days' in blank, false);
  assert.equal('max_age_days' in blank, false);
});

// ==================== ② 库龄区间校验 ====================

test('区间：合法 / 单边 / 全空都通过', () => {
  assert.deepEqual(ageRangeOf('', ''), { min: undefined, max: undefined });
  assert.deepEqual(ageRangeOf('0', ''), { min: 0, max: undefined });
  assert.deepEqual(ageRangeOf('', '90'), { min: undefined, max: 90 });
  assert.deepEqual(ageRangeOf('31', '90'), { min: 31, max: 90 });
});

test('区间反了 / 负数 / 小数都要拦住，并且点名两个值', () => {
  const reversed = ageRangeOf('90', '30');
  assert.ok(reversed.error, '反区间必须有 error');
  assert.ok(reversed.error.includes('90') && reversed.error.includes('30'), `要点名两个值：${reversed.error}`);

  assert.ok(ageRangeOf('-1', '').error?.includes('最小库龄'));
  assert.ok(ageRangeOf('', '-5').error?.includes('最大库龄'));
  assert.ok(ageRangeOf('1.5', '').error, '小数不是整数天，要拒');
  assert.ok(ageRangeOf('abc', '').error);
});

// ==================== ③ 数值 / 比例格式化 ====================

test('数量：后端字符串 Decimal 也要格式化掉尾零', () => {
  assert.equal(formatQty('120.0000'), '120');
  assert.equal(formatQty('0.5000'), '0.5');
  assert.equal(formatQty(0), '0');
  assert.equal(formatQty(null), '0');
  assert.equal(formatQty(undefined), '0');
  assert.equal(formatQty('12.3456'), '12.3456');
});

test('比例：后端 4 位小数（0.1538）→ 15.38%，不再除一次 100', () => {
  assert.equal(formatRatioPercent('0.1538'), '15.38%');
  assert.equal(formatRatioPercent(0), '0.00%');
  assert.equal(formatRatioPercent('1'), '100.00%');
});

test('库龄负数照实说（后端不夹 0）', () => {
  assert.equal(ageDaysText(-15), '-15（晚于基准日）');
  assert.equal(ageDaysText(0), '0');
  assert.equal(ageDaysText(91), '91');
});

test('库龄分级：60/61/90/91 的天花板与地板', () => {
  assert.equal(ageTheme(0), 'default');
  assert.equal(ageTheme(60), 'default');
  assert.equal(ageTheme(61), 'warning');
  assert.equal(ageTheme(90), 'warning');
  assert.equal(ageTheme(91), 'danger');
  assert.equal(ageTheme(3650), 'danger');
});

test('呆滞占比分级：≥50% 危险、>0 提醒', () => {
  assert.equal(slowRatioTheme('0'), 'default');
  assert.equal(slowRatioTheme('0.0001'), 'warning');
  assert.equal(slowRatioTheme('0.4999'), 'warning');
  assert.equal(slowRatioTheme('0.5'), 'danger');
  assert.equal(slowRatioTheme('1'), 'danger');
});

// ==================== ④ 口径文案 ====================

test('基准日来源 / 阈值来源都说人话', () => {
  assert.equal(asOfSourceText('QUERY'), '手动指定');
  assert.equal(asOfSourceText('TODAY'), '今天（CST）');
  assert.equal(asOfSourceText('whatever'), '今天（CST）');

  assert.equal(thresholdSourceText('QUERY'), '本次手填');
  assert.equal(thresholdSourceText('CONFIG'), '系统参数');
  assert.equal(thresholdSourceText('DEFAULT'), '缺省');
});

test('阈值提示：生效值 + 来源 + 参数键三样都在', () => {
  const report = {
    threshold_days: 180,
    threshold_source: 'CONFIG',
    threshold_config_key: SLOW_MOVING_CONFIG_KEY,
  } as SlowMovingReport;
  const hint = thresholdHint(report);
  assert.ok(hint.includes('180 天'));
  assert.ok(hint.includes('系统参数'));
  assert.ok(hint.includes(SLOW_MOVING_CONFIG_KEY));
  assert.equal(SLOW_MOVING_DEFAULT_DAYS, 90, '缺省阈值与后端 DEFAULT_THRESHOLD_DAYS 一致');
  assert.equal(SLOW_MOVING_CONFIG_KEY, 'erp.inventory.slow-moving-days');
});

test('截断提示：没截断返回空串；截断了要说「合计仍是全量」', () => {
  assert.equal(truncatedNote(false, 2500, 2000), '');
  const note = truncatedNote(true, 2500, 2000);
  assert.ok(note.includes('2000') && note.includes('2500'), note);
  assert.ok(note.includes('合计'), `要说明合计不受影响：${note}`);
});

test('空态文案分「筛过」与「没筛过」两种', () => {
  const report = { as_of: '2026-11-30' } as InventoryAgingReport;
  const slow = { as_of: '2026-11-30', threshold_days: 90 } as SlowMovingReport;

  assert.ok(reportEmptyText(report, false).includes('2026-11-30'));
  assert.ok(reportEmptyText(report, true).includes('没有匹配'));

  assert.ok(slowEmptyText(slow, false).includes('90'), '呆滞空态要带上阈值天数');
  assert.ok(slowEmptyText(slow, true).includes('没有呆滞'));

  assert.ok(layerEmptyText(false).includes('入库层'));
  assert.ok(layerEmptyText(true).includes('没有匹配'));
});

test('403 文案要点名缺哪个码（否则用户以为系统坏了）', () => {
  const message = requestErrorMessage(new Error('403 无权限'));
  assert.ok(message.includes(PERM_AGING_REPORT));
  assert.ok(message.includes(PERM_AGING_SLOW_MOVING));
  assert.ok(message.includes(PERM_AGING_LIST));

  // 非权限错误原样透出，不吞
  assert.equal(requestErrorMessage(new Error('网络断开')), '网络断开');
});

// ==================== ⑤ 行键 / 钻取 / 主数据文本 ====================

test('报表行键 = 物料|仓库（同一物料多个仓库不能撞 key）', () => {
  assert.equal(agingRowKey({ material_id: 'm', warehouse_id: 'w' }), 'm|w');
  assert.notEqual(agingRowKey({ material_id: 'm', warehouse_id: 'w' }), agingRowKey({ material_id: 'm', warehouse_id: 'w2' }));
});

test('钻取：只带 物料 × 仓库 + 基准日，库龄区间清空', () => {
  const state = drillLayerFilter({ material_id: 'm', warehouse_id: 'w' }, '2026-11-30');
  assert.equal(state.material_id, 'm');
  assert.equal(state.warehouse_id, 'w');
  assert.equal(state.as_of, '2026-11-30');
  assert.equal(state.min_age_days, '');
  assert.equal(state.max_age_days, '');
  assert.equal(state.include_consumed, false);
  assert.equal(state.lot_no, '');
});

test('筛过没筛过：报表 / 批次层各自判断', () => {
  assert.equal(isAgingFiltered(emptyAgingFilter()), false);
  assert.equal(isAgingFiltered({ ...emptyAgingFilter(), as_of: '2026-11-30' }), true);
  assert.equal(isAgingFiltered({ ...emptyAgingFilter(), material_id: 'm' }), true);

  assert.equal(isLayerFiltered(emptyLayerFilter()), false);
  assert.equal(isLayerFiltered({ ...emptyLayerFilter(), lot_no: 'LOT-1' }), true);
  assert.equal(isLayerFiltered({ ...emptyLayerFilter(), min_age_days: 0 }), true, '0 天是合法筛选（不是「没填」）');
  assert.equal(isLayerFiltered({ ...emptyLayerFilter(), include_consumed: true }), true);
});

test('主数据软删时点名，不显示 UUID 也不留白', () => {
  assert.equal(materialText({ material_code: 'M001', material_name: '螺丝' }), 'M001 螺丝');
  assert.equal(materialText({ material_code: null, material_name: null }), '（物料已删除）');
  assert.equal(warehouseText({ warehouse_code: 'W1', warehouse_name: '一号仓' }), 'W1 一号仓');
  assert.equal(warehouseText({ warehouse_code: null, warehouse_name: null }), '（仓库已删除）');
});

test('单据 / 业务类型：认识的翻中文，不认识的**原样返回**（不藏后端新增类型）', () => {
  assert.equal(docTypeText('STOCK_IN'), '入库单');
  assert.equal(docTypeText('OPENING'), '期初结转');
  assert.equal(docTypeText('BRAND_NEW_TYPE'), 'BRAND_NEW_TYPE');
  assert.equal(bizTypeText('PURCHASE_IN'), '采购入库');
  assert.equal(bizTypeText('SOMETHING_ELSE'), 'SOMETHING_ELSE');
});

test('跨单位提示：说清数量合计只作参考、金额是参考金额', () => {
  const note = reportTotalNote();
  assert.ok(note.includes('单位'));
  assert.ok(note.includes('参考'));
});

test('档位按 key 取（不写下标）：缺档返回 undefined 而不是 0', () => {
  const cells = [
    { key: 'D0_30', label: '0-30 天', layer_count: 1, qty: '10', amount: '20' },
    { key: 'OVER_90', label: '90 天以上', layer_count: 0, qty: '0', amount: '0' },
  ];
  assert.equal(bucketCellOf(cells, 'OVER_90')?.qty, '0');
  assert.equal(bucketCellOf(cells, 'D31_60'), undefined);
});

test('权限码 / Tab 常量与迁移逐字一致', () => {
  assert.equal(PERM_AGING_PAGE, 'business:erp:inventory-aging:page');
  assert.equal(PERM_AGING_LIST, 'business:erp:inventory-aging:list');
  assert.equal(PERM_AGING_REPORT, 'business:erp:inventory-aging:report');
  assert.equal(PERM_AGING_SLOW_MOVING, 'business:erp:inventory-aging:slow-moving');
  assert.deepEqual(
    AGING_TABS.map((tab) => tab.key),
    ['report', 'slow', 'layers'],
  );
  assert.equal(AGING_PAGE_SIZE, 20);
});

// ==================== ⑥ 源码即契约 ====================

test('API 层：三条路径都带 /api 前缀，且指向同一个模块', () => {
  assert.ok(API.includes('/api/business/erp'));
  assert.ok(API.includes('${BASE}/inventory-aging/page`'));
  assert.ok(API.includes('${BASE}/inventory-aging/report`'));
  assert.ok(API.includes('${BASE}/inventory-aging/slow-moving`'));
  // 空值整键缺席的兜底必须存在（否则 ?material_id= 会让后端 400）
  assert.ok(withoutComments(API).includes('value === \'\''));
});

test('页面：合计只认后端 summary，绝不把 rows 自己加一遍', () => {
  const code = withoutComments(PAGE);
  assert.ok(TPL.includes('report.summary'), '合计行必须取后端 summary');
  assert.ok(TPL.includes('slow.summary'), '呆滞 KPI 必须取后端 summary');
  assert.equal(code.includes('.reduce('), false, '页面不许出现客户端累加（第二套账，截断时必然算小）');
});

test('页面：三个 Tab 各自按取数码决定是否渲染（有页面码但缺数据码时不硬画）', () => {
  const code = withoutComments(PAGE);
  assert.ok(code.includes('hasPermission(PERM_AGING_REPORT)'), '报表 Tab 要判报表码');
  assert.ok(code.includes('hasPermission(PERM_AGING_SLOW_MOVING)'), '呆滞 Tab 要判呆滞码');
  assert.ok(code.includes('hasPermission(PERM_AGING_LIST)'), '批次层 Tab 要判取数码');
  assert.ok(TPL.includes('v-if="canReport"'));
  assert.ok(TPL.includes('v-if="canSlow"'));
  assert.ok(TPL.includes('v-if="canLayers"'));
});

test('页面：档位列由后端 buckets 驱动（不写死 4 列）', () => {
  assert.ok(TPL.includes('v-for="bucket in report.buckets"'), '表头必须 v-for 后端 buckets');
  assert.ok(TPL.includes('bucketCellOf(row.buckets, bucket.key)'), '行单元格按 key 取');
});

test('页面：批次层 Tab 走 ListPage，区间写反时不发请求', () => {
  const code = withoutComments(PAGE);
  assert.ok(TPL.includes('<ListPage'), '批次层用 ListPage（分页 / 筛选 / 空态统一）');
  assert.ok(code.includes('throw new Error(rangeError)'), '区间写反要在 queryBuilder 里拦住');
  assert.ok(TPL.includes('aging-layer-range-error'), '区间写反要有提示');
});

test('路由：path 与迁移 sys_menu.path 逐字一致，meta.permission 用页面显形码', () => {
  const route = ROUTER.slice(ROUTER.indexOf("path: 'business/erp/inventory-aging'"));
  const head = route.slice(0, 600);
  assert.ok(head.includes("name: 'BusinessErpInventoryAging'"));
  assert.ok(head.includes('@/views/business/erp/inventory-aging/index.vue'));
  assert.ok(head.includes(`permission: '${PERM_AGING_PAGE}'`));
});

test('迁移：页面行字段齐全（码 / path / 组件 / 父目录 / sort / title / icon）', () => {
  assert.ok(MIG_UP.includes("'20000000-0000-0000-0000-000000004270'::uuid"));
  assert.ok(MIG_UP.includes("'business:erp:inventory-aging:page'"));
  assert.ok(MIG_UP.includes("'/business/erp/inventory-aging'"));
  assert.ok(MIG_UP.includes("'business/erp/inventory-aging/index'"));
  assert.ok(MIG_UP.includes("'20000000-0000-0000-0000-000000000e01'::uuid"), '挂在「仓库管理」目录下');
  assert.ok(MIG_UP.includes("'chart-bar'"));
  assert.ok(MIG_UP.includes("'库龄分析'"));
  // 幂等与自检：ON CONFLICT DO NOTHING + 一码一节点 + 同父 sort 唯一 + 祖先闭包
  assert.ok(MIG_UP.includes('ON CONFLICT (id) DO NOTHING'));
  assert.ok(MIG_UP.includes('business:erp:inventory-aging:page 在 sys_menu 里出现'));
  assert.ok(MIG_UP.includes('sort_order'));
  assert.ok(MIG_UP.includes('祖先目录'));
  // 回滚只删本迁移种下的行
  assert.ok(MIG_DOWN.includes("'20000000-0000-0000-0000-000000004270'::uuid"));
  assert.ok(MIG_DOWN.includes('sys_role_menu'));
  assert.equal(MIG_DOWN.includes("'20000000-0000-0000-0000-000000004240'::uuid"), false, 'down 不许碰取数按钮行');
});

test('页面显形码不进后端：本页只在 API 层与路由里出现', () => {
  // 页面码是「显形码」：后端源码零引用（permission_audit_tests::ROUTE_LEVEL_ONLY 登记）。
  // 这里守前端侧：三个接口的权限码常量必须是取数码，不是页面码。
  const pageCodeUsages = LOGIC.split('\n').filter((line) => line.includes('PERM_AGING_PAGE'));
  assert.ok(pageCodeUsages.length >= 1, '页面码常量必须有定义');
  assert.equal(API.includes('inventory-aging:page'), false, 'API 层不许出现页面码（接口判取数码）');
});
