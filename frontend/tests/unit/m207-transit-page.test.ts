/**
 * M2-07：在途库存页**页面**契约单测（页面主体为新落地）。
 *
 * 运行：`cd frontend && pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 两层断言
 *
 * 1. **纯逻辑**（`transit-logic.ts`）：拼参（`status` 缺省必须整键缺席、日期区间拆两端）、
 *    数量格式化（后端 `Decimal` 出的是字符串）、账龄分级、空态与 403 人话。
 *    这些是「页面把账看错 / 把筛选看丢」的入口。
 * 2. **源码即契约**（`index.vue` / `router/index.ts` / 迁移 `V0423`）：真正会静默出错的是
 *    「模板里调了谁」—— 例如把合计改成客户端加总、页面行 auth_code 与取数码合并成一行、
 *    路由 path 与迁移 `sys_menu.path` 漂了（点菜单 404）。逻辑函数没变、逻辑单测全绿，页面却错了。
 *
 * 只读源码做子串 / 正则断言，不引 DOM、不引浏览器（与 `m123-receivable-aging-page.test.ts` 同款）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  AGE_DANGER_DAYS,
  AGE_WARN_DAYS,
  PERM_TRANSIT_AUDIT,
  PERM_TRANSIT_LIST,
  PERM_TRANSIT_PAGE,
  TRANSIT_PAGE_SIZE,
  TRANSIT_STATUS_OPTIONS,
  TRANSIT_TABS,
  ageText,
  ageTheme,
  buildSummaryQuery,
  buildTransitQuery,
  customerText,
  detailEmptyText,
  emptySummaryFilter,
  emptyTransitFilter,
  formatQty,
  isPositiveQty,
  isSummaryFiltered,
  isTransitFiltered,
  materialText,
  requestErrorMessage,
  statusLabel,
  statusTheme,
  summaryEmptyText,
  truncatedNote,
  warehouseText,
} from '@/views/business/erp/inventory-transit/transit-logic';
import type { TransitSummary } from '@/api/business/erp/inventory-transit';

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

const PAGE_PATH = 'src/views/business/erp/inventory-transit/index.vue';
const LOGIC_PATH = 'src/views/business/erp/inventory-transit/transit-logic.ts';
const API_PATH = 'src/api/business/erp/inventory-transit.ts';
const ROUTER_PATH = 'src/router/index.ts';
const MIG_UP_PATH = '../backend/app/migrations/business/V0423__erp_inventory_transit_page.up.sql';
const MIG_DOWN_PATH = '../backend/app/migrations/business/V0423__erp_inventory_transit_page.down.sql';

const PAGE = sourceOf(PAGE_PATH);
const PAGE_CODE = withoutComments(PAGE);
const TPL = templateOf(PAGE);
const LOGIC = sourceOf(LOGIC_PATH);
const API = sourceOf(API_PATH);
const ROUTER = sourceOf(ROUTER_PATH);
const MIG_UP = sourceOf(MIG_UP_PATH);
const MIG_DOWN = sourceOf(MIG_DOWN_PATH);

// ==================== ① 拼参：status 缺省必须整键缺席 ====================

test('明细拼参：空筛选只发分页（status 不传 = 后端默认只看在途）', () => {
  const query = buildTransitQuery(emptyTransitFilter());
  assert.deepEqual(query, { page: 1, page_size: TRANSIT_PAGE_SIZE });
  assert.equal('status' in query, false, 'status 留空必须整键缺席：发 ?status= 会变成「等于空串」查出 0 行');
  assert.equal('ship_date_from' in query, false);
  assert.equal('ship_date_to' in query, false);
});

test('明细拼参：显式选「已签收」才发 status', () => {
  const filter = { ...emptyTransitFilter(), status: 'RECEIVED' as const };
  assert.equal(buildTransitQuery(filter).status, 'RECEIVED');
});

test('明细拼参：日期区间拆成 ship_date_from / ship_date_to', () => {
  const filter = { ...emptyTransitFilter(), date_range: ['2026-11-01', '2026-11-16'] };
  const query = buildTransitQuery(filter);
  assert.equal(query.ship_date_from, '2026-11-01');
  assert.equal(query.ship_date_to, '2026-11-16');
});

test('明细拼参：只给区间起点时只发 from（「从某天起」是常用查法）', () => {
  const filter = { ...emptyTransitFilter(), date_range: ['2026-11-01', ''] };
  const query = buildTransitQuery(filter);
  assert.equal(query.ship_date_from, '2026-11-01');
  assert.equal('ship_date_to' in query, false);
});

test('明细拼参：空串整键缺席 + 页码兜底', () => {
  const filter = {
    ...emptyTransitFilter(),
    material_id: '',
    warehouse_id: 'w-1',
    customer_id: '',
    shipment_id: '',
  };
  const query = buildTransitQuery(filter, 0, 0);
  assert.equal(query.page, 1);
  assert.equal(query.page_size, TRANSIT_PAGE_SIZE);
  assert.equal(query.warehouse_id, 'w-1');
  assert.equal('material_id' in query, false);
  assert.equal('customer_id' in query, false);
  assert.equal('shipment_id' in query, false);
});

test('汇总拼参：没有 status / 没有分页，空串整键缺席', () => {
  const query = buildSummaryQuery({ material_id: 'm-1', warehouse_id: '', customer_id: '' });
  assert.deepEqual(query, { material_id: 'm-1' });
  assert.equal('status' in query, false);
  assert.equal('page' in query, false);
});

test('筛选生效判定：明细六个条件任一非空即算在筛；汇总只看三个', () => {
  assert.equal(isTransitFiltered(emptyTransitFilter()), false);
  assert.equal(isTransitFiltered({ ...emptyTransitFilter(), status: 'IN_TRANSIT' }), true);
  assert.equal(isTransitFiltered({ ...emptyTransitFilter(), date_range: ['2026-11-01'] }), true);
  assert.equal(isTransitFiltered({ ...emptyTransitFilter(), date_range: ['', ''] }), false);
  assert.equal(isSummaryFiltered(emptySummaryFilter()), false);
  assert.equal(isSummaryFiltered({ ...emptySummaryFilter(), customer_id: 'c-1' }), true);
});

// ==================== ② 数量：后端 Decimal 是字符串 ====================

test('数量格式化：去尾零 + 千分位，不把 4 位小数口径抹成 2 位', () => {
  assert.equal(formatQty('6.0000'), '6');
  assert.equal(formatQty('6.5000'), '6.5');
  assert.equal(formatQty('1234.1250'), '1,234.125');
  assert.equal(formatQty(1234567), '1,234,567');
  assert.equal(formatQty('0.0000'), '0');
  assert.equal(formatQty('-0.0000'), '0');
  assert.equal(formatQty(null), '0');
  assert.equal(formatQty(undefined), '0');
  assert.equal(formatQty(''), '0');
});

test('数量格式化：不认识的形态原样返回（宁可显示 6.0000 也不显示 0）', () => {
  assert.equal(formatQty('abc'), 'abc');
});

test('在途量 > 0 判定：字符串 "0.0000" 是 0', () => {
  assert.equal(isPositiveQty('0.0000'), false);
  assert.equal(isPositiveQty('0.0001'), true);
  assert.equal(isPositiveQty(0), false);
  assert.equal(isPositiveQty(null), false);
});

// ==================== ③ 账龄 / 状态判定 ====================

test('账龄分级边界：7 天正常 / 8 天关注 / 30 天关注 / 31 天警示', () => {
  assert.equal(AGE_WARN_DAYS, 8);
  assert.equal(AGE_DANGER_DAYS, 31);
  assert.equal(ageTheme(0), 'success');
  assert.equal(ageTheme(7), 'success');
  assert.equal(ageTheme(8), 'warning');
  assert.equal(ageTheme(30), 'warning');
  assert.equal(ageTheme(31), 'danger');
});

test('账龄文案：后端没给值显示「—」，不显示 0 天（0 天 = 今天发的，是有效信息）', () => {
  assert.equal(ageText(0), '0 天');
  assert.equal(ageText(12), '12 天');
  assert.equal(ageText(null), '—');
  assert.equal(ageText(undefined), '—');
});

test('状态中文名：优先用后端 status_label，兜底表覆盖三值', () => {
  assert.equal(statusLabel('IN_TRANSIT', '后端给的名字'), '后端给的名字');
  assert.equal(statusLabel('IN_TRANSIT'), '在途（未签收）');
  assert.equal(statusLabel('RECEIVED'), '已签收');
  assert.equal(statusLabel('CANCELLED'), '已作废');
});

test('状态主题：在途橙 / 已签收绿 / 已作废红', () => {
  assert.equal(statusTheme('IN_TRANSIT'), 'warning');
  assert.equal(statusTheme('RECEIVED'), 'success');
  assert.equal(statusTheme('CANCELLED'), 'danger');
  assert.equal(statusTheme(null), 'default');
});

test('状态下拉三项，且**没有**空串「全部」（空白 = 未选，TDesign 会显示 placeholder）', () => {
  assert.equal(TRANSIT_STATUS_OPTIONS.length, 3);
  assert.deepEqual(
    TRANSIT_STATUS_OPTIONS.map((item) => item.value),
    ['IN_TRANSIT', 'RECEIVED', 'CANCELLED'],
  );
  assert.equal(
    TRANSIT_STATUS_OPTIONS.some((item) => String(item.value) === ''),
    false,
  );
});

// ==================== ④ 主数据软删时的展示 ====================

test('物料 / 客户 / 仓库名字：主数据软删时点名，绝不回落到 UUID', () => {
  assert.equal(materialText({ material_code: 'M-001', material_name: '螺丝' }), 'M-001 螺丝');
  assert.equal(materialText({ material_code: 'M-001', material_name: null }), 'M-001');
  assert.equal(materialText({ material_code: null, material_name: null }), '（物料已删除）');
  assert.equal(customerText({ customer_code: 'C-1', customer_name: '甲公司' }), 'C-1 甲公司');
  assert.equal(customerText({ customer_code: null, customer_name: null }), '（客户已删除）');
  assert.equal(warehouseText({ warehouse_code: 'W-1', warehouse_name: '主仓' }), 'W-1 主仓');
  assert.equal(warehouseText({ warehouse_code: null, warehouse_name: '' }), '（仓库已删除）');
});

// ==================== ⑤ 空态 / 截断 / 403 人话 ====================

test('明细空态：筛选生效与「本来就没有在途」是两句不同的话', () => {
  const plain = detailEmptyText(emptyTransitFilter());
  const filtered = detailEmptyText({ ...emptyTransitFilter(), status: 'RECEIVED' });
  assert.notEqual(plain.title, filtered.title);
  assert.match(plain.desc, /发货过账/);
  assert.match(filtered.title, /筛选/);
});

test('汇总空态：说明「全部签收后这一页就是空的」= 正常形态，不是数据缺失', () => {
  const plain = summaryEmptyText(emptySummaryFilter());
  assert.match(plain.desc, /正常/);
  assert.match(summaryEmptyText({ ...emptySummaryFilter(), material_id: 'm' }).title, /筛选/);
});

function summaryOf(truncated: boolean): TransitSummary {
  return {
    rows: [],
    row_count: truncated ? 500 : 0,
    truncated,
    total_qty_in_transit: '0',
    total_shipments: 0,
    total_lines: 0,
  };
}

test('截断提示：未截断不提示；截断时必须同时说清「合计仍是全量」', () => {
  assert.equal(truncatedNote(null), '');
  assert.equal(truncatedNote(summaryOf(false)), '');
  const note = truncatedNote(summaryOf(true));
  assert.match(note, /500/);
  assert.match(note, /全量/);
});

test('403 人话点名缺哪个码（页面码与取数码分开授予的典型错配）', () => {
  const text = requestErrorMessage(new Error('403 无权限'));
  assert.ok(text.includes(PERM_TRANSIT_LIST), '403 人话要点名取数码');
  assert.ok(text.includes(PERM_TRANSIT_PAGE), '403 人话要点名页面码');
  assert.match(text, /角色/);
});

test('其它错误原样透出（不要把 400 的「参数非法」也变成权限话术）', () => {
  assert.equal(requestErrorMessage(new Error('status 不在 3 值域内')), 'status 不在 3 值域内');
  assert.match(requestErrorMessage(null), /没有返回原因/);
});

test('对账码常量存在但本页不消费（对账是 M2-05 的另一面，另一张单）', () => {
  assert.equal(PERM_TRANSIT_AUDIT, 'business:erp:inventory-transit:audit');
  assert.equal(PAGE.includes('PERM_TRANSIT_AUDIT'), false, '在途页不应对账：本卡只做明细 + 汇总');
});

// ==================== ⑥ 页面源码契约 ====================

test('页面两个 Tab：明细 + 汇总，且 tab 键与 TRANSIT_TABS 一致', () => {
  assert.equal(TRANSIT_TABS.length, 2);
  assert.match(TPL, /TRANSIT_TABS\[0\]\.key/);
  assert.match(TPL, /TRANSIT_TABS\[1\]\.key/);
  assert.match(PAGE, /在途明细/);
  assert.match(PAGE, /在途汇总/);
  // 汇总面板 lazy：切过去才拉，别在首屏白打一次请求
  assert.match(TPL, /:value="TRANSIT_TABS\[1\]\.key"[^>]*lazy/);
});

test('明细走 ListPage（筛选 / 分页 / 空态 / 错误态不自己造一套）', () => {
  assert.match(TPL, /<ListPage/);
  assert.match(TPL, /:page-size="TRANSIT_PAGE_SIZE"/);
  assert.match(PAGE, /:fetcher="detailFetcher"/);
  assert.match(PAGE, /:query-builder="detailQueryBuilder"/);
  assert.match(PAGE, /fetchTransitPage/);
});

test('汇总走 /summary 且**不做客户端加总**（前端加一遍 = 第二套账，截断时必然算小）', () => {
  assert.match(PAGE, /fetchTransitSummary/);
  assert.match(TPL, /summary\.total_qty_in_transit/);
  assert.match(TPL, /summary\.total_shipments/);
  assert.match(TPL, /summary\.total_lines/);
  assert.equal(/\.reduce\s*\(/.test(PAGE_CODE), false, '页面不许出现 reduce：合计一律取后端 total_*');
});

test('页面只读：不引任何写接口（在途行由发货过账 / 签收产生与结转）', () => {
  assert.equal(/\b(post|patch|put|del)\s*[<(]/.test(withoutComments(API)), false);
  assert.equal(/createTransit|updateTransit|deleteTransit/.test(PAGE_CODE), false);
});

test('API 路径写全 /api（漏前缀会命中 SPA fallback 返回 HTML）', () => {
  assert.match(API, /const BASE = '\/api\/business\/erp'/);
  assert.match(API, /inventory-transit\/page/);
  assert.match(API, /inventory-transit\/summary/);
});

// ==================== ⑦ 路由 / 菜单 / 迁移三方一致 ====================

test('路由已注册，path 与迁移 sys_menu.path 逐字一致（否则点菜单 404）', () => {
  assert.ok(
    ROUTER.includes("path: 'business/erp/inventory-transit'"),
    'router/index.ts 缺在途库存路由（n08 会判红：点菜单必然 404）',
  );
  assert.ok(ROUTER.includes(`permission: '${PERM_TRANSIT_PAGE}'`), '路由 meta.permission 必须用页面显形码');
  assert.ok(MIG_UP.includes("'/business/erp/inventory-transit'"), '迁移里的 path 与路由不一致');
});

test('迁移种的是**页面行 + 页面码**，取数码那行一个字都没动', () => {
  assert.match(MIG_UP, new RegExp(PERM_TRANSIT_PAGE.replace(/[-:]/g, (ch) => `\\${ch}`)));
  assert.match(MIG_UP, /menu_type[^)]*2/s, '页面行必须是 menu_type=2');
  assert.ok(MIG_UP.includes("'business/erp/inventory-transit/index'"), 'component 必须指向前端页面目录');
  assert.ok(MIG_UP.includes("'在途库存'"), 'title 必须是在途库存');
  assert.match(MIG_UP, /sort_order[\s\S]{0,80}\b45\b/, 'sort_order 必须显式给 45（同父唯一）');
  // 取数码的行由 V0421 持有：本迁移只允许 SELECT 它，不许 UPDATE / DELETE / 再 INSERT 一行同码
  assert.equal(/DELETE\s+FROM\s+sys_menu\s+WHERE[\s\S]{0,200}inventory-transit:list/i.test(MIG_UP), false);
  assert.equal(/INSERT\s+INTO\s+sys_menu[\s\S]{0,400}'business:erp:inventory-transit:list'/i.test(MIG_UP), false);
});

test('迁移授权成对：页面码跟取数码的持有角色走 + 超管 + 祖先闭包', () => {
  assert.ok(MIG_UP.includes(`'${PERM_TRANSIT_LIST}'`), '授权段必须引用取数码（看得到 ⇒ 调得动）');
  assert.ok(MIG_UP.includes("role_code = 'SUPER_ADMIN'"), '超管要显式补一行（权限树可读）');
  assert.ok(MIG_UP.includes("'20000000-0000-0000-0000-000000000e01'"), '必须补祖先目录（缺父目录侧栏整棵子树消失）');
  assert.match(MIG_UP, /自检/, '迁移必须有自检块');
});

test('迁移回滚只删本卡种下的行（授权 → 菜单行），且幂等（DELETE）', () => {
  assert.match(MIG_DOWN, /DELETE FROM sys_role_menu/);
  assert.match(MIG_DOWN, /DELETE FROM sys_menu/);
  assert.ok(MIG_DOWN.includes('20000000-0000-0000-0000-000000004230'));
});

test('前端 API / 逻辑 / 页面三处字段名与后端一致（qty_in_transit 是生成列）', () => {
  for (const field of ['qty_in_transit', 'shipped_qty', 'received_qty', 'age_days', 'first_ship_date', 'max_age_days']) {
    assert.ok(API.includes(field), `API 类型缺字段 ${field}`);
  }
  assert.ok(LOGIC.includes('qty_in_transit'), '逻辑层必须按后端字段名读取在途量');
});
