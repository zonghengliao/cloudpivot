/**
 * 盘点单页纯逻辑单测（DC-14 前端补齐，卡 t_71b01775）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零依赖、无 DOM）。
 *
 * 测 `.ts` 不测 `.vue` 的原因同 `stock-out-logic`：用例跑在没有 DOM 的 node 里，
 * 而「按钮该不该出、全盘能不能带 items、实盘数怎么发」全在逻辑层；
 * `.vue` 只负责渲染，这些一旦漂移 `vue-tsc` 抓不到。
 *
 * 断言口径照后端抄（`backend/src/business/erp/stocktake_{model,dto}.rs`）：
 * - 类型两值 `TAKE_TYPES = [FULL, PARTIAL]`（全盘 / 抽盘）；
 * - 状态四值 `STATUS_*`，边表 `STOCK_TAKE_EDGES` **没有** `DRAFT → POSTED`；
 * - 全盘不接受 items、抽盘 1–2000 行（`MAX_LINES = 2000`）；
 * - 建单范围只有 `material_id / location_id`（账面数由服务端快照）；
 * - 没配流程是 409 + `66033`，跳过审批要 `business:erp:skip-approval` + 理由 ≥ 10 字。
 *
 * 最后两条用例是**前端侧对 n08 的镜像**：n08 从 `sys_menu` 读菜单 path，再逐条比对
 * `router/index.ts` 里 `path: '...'` 的解析结果；这里反过来钉住「盘点单这条菜单的
 * path 与视图文件都真的存在」，防止哪天有人删了路由却以为菜单还能点。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  CODE_FLOW_NOT_CONFIGURED,
  MIN_SKIP_REASON_CHARS,
  PERM_SKIP_APPROVAL,
  STOCK_TAKE_MAX_LINES,
  STOCK_TAKE_MAX_REASON,
  STOCK_TAKE_PAGE_SIZE,
  STOCK_TAKE_STATUS_FILTER_OPTIONS,
  TAKE_TYPE_OPTIONS,
  aggregateDiff,
  gainLossDisplay,
  buildActualPayload,
  buildCancelPayload,
  buildSkipSubmitPayload,
  buildStockTakePayload,
  buildStockTakeQuery,
  canDelete,
  canEdit,
  detailToActuals,
  diffDirection,
  emptyStockTakeFilter,
  emptyStockTakeForm,
  errorText,
  formatDiff,
  isFlowNotConfigured,
  isStockTakeFiltered,
  statusActions,
  statusHistory,
  stockTakeStatusLabel,
  stockTakeStatusTheme,
  takeTypeLabel,
  todayString,
  validateActuals,
  validateCancelReason,
  validateSkipReason,
  validateStockTakeForm,
  type StockTakeFormState,
} from '../../src/views/business/erp/stock-take/stock-take-logic.ts';
import type { StockTakeDetail, StockTakeItemLine } from '../../src/api/business/erp/stock-take.ts';

/** 一个填得完全合法的抽盘单（各用例在此基础上只改一处） */
function validPartialForm(): StockTakeFormState {
  const form = emptyStockTakeForm(new Date('2026-11-04T02:00:00Z'));
  form.take_type = 'PARTIAL';
  form.warehouse_id = 'wh-1';
  form.items = [{ key: 'line-1', material_id: 'mat-1' }];
  return form;
}

/** 一条明细（详情用例用） */
function line(id: string, lineNo: number, book: string, actual: string): StockTakeItemLine {
  return {
    id,
    line_no: lineNo,
    material_id: `mat-${lineNo}`,
    material_code: `M-00${lineNo}`,
    material_name: `物料${lineNo}`,
    unit_id: 'unit-1',
    location_id: null,
    book_qty: book,
    actual_qty: actual,
    diff_qty: String(Number(actual) - Number(book)),
    unit_cost: '0',
    diff_amount: '0',
    remark: null,
  };
}

// ==================== 枚举 / 译名 ====================

test('盘点类型两值与中文名逐字对齐后端 TAKE_TYPES', () => {
  assert.deepEqual(
    TAKE_TYPE_OPTIONS.map((item) => item.value),
    ['FULL', 'PARTIAL'],
  );
  assert.deepEqual(
    TAKE_TYPE_OPTIONS.map((item) => item.label),
    ['全盘', '抽盘'],
  );
  assert.equal(takeTypeLabel('FULL'), '全盘');
  assert.equal(takeTypeLabel('PARTIAL'), '抽盘');
  assert.equal(takeTypeLabel('OTHER'), 'OTHER');
});

test('状态四值的译名与标签主题', () => {
  assert.deepEqual(
    STOCK_TAKE_STATUS_FILTER_OPTIONS.map((item) => item.value),
    ['DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED'],
  );
  assert.equal(stockTakeStatusLabel('DRAFT'), '草稿');
  assert.equal(stockTakeStatusLabel('PENDING_POST'), '待过账');
  assert.equal(stockTakeStatusLabel('POSTED'), '已过账');
  assert.equal(stockTakeStatusLabel('CANCELLED'), '已作废');
  assert.equal(stockTakeStatusTheme('PENDING_POST'), 'warning');
  assert.equal(stockTakeStatusTheme('POSTED'), 'success');
  assert.equal(stockTakeStatusTheme('CANCELLED'), 'danger');
});

// ==================== 建单表单 ====================

test('空表单默认全盘 + 今天（CST）+ 无范围行', () => {
  const form = emptyStockTakeForm(new Date('2026-11-04T18:30:00Z'));
  assert.equal(form.take_type, 'FULL');
  // UTC 18:30 = 北京时间次日 02:30
  assert.equal(form.take_date, '2026-11-05');
  assert.equal(form.items.length, 0);
  assert.equal(todayString(new Date('2026-11-04T02:00:00Z')), '2026-11-04');
});

test('全盘不填范围也能通过校验（范围由服务端快照）', () => {
  const form = emptyStockTakeForm(new Date('2026-11-04T02:00:00Z'));
  form.warehouse_id = 'wh-1';
  assert.deepEqual(validateStockTakeForm(form), {});
});

test('全盘带 items 必须报错（后端对 FULL + items 直接 400）', () => {
  const form = emptyStockTakeForm(new Date('2026-11-04T02:00:00Z'));
  form.warehouse_id = 'wh-1';
  form.items = [{ key: 'line-1', material_id: 'mat-1' }];
  const errors = validateStockTakeForm(form);
  assert.ok(errors.items, '全盘带范围行必须被拦住');
  assert.match(errors.items, /全盘/);
});

test('抽盘没有范围行必须报错', () => {
  const form = emptyStockTakeForm(new Date('2026-11-04T02:00:00Z'));
  form.take_type = 'PARTIAL';
  form.warehouse_id = 'wh-1';
  const errors = validateStockTakeForm(form);
  assert.ok(errors.items);
  assert.match(errors.items, /至少 1 行/);
});

test('抽盘同一物料同一库位重复 → 报错（否则过账双倍调平）', () => {
  const form = validPartialForm();
  form.items = [
    { key: 'line-1', material_id: 'mat-1', location_id: 'loc-1' },
    { key: 'line-2', material_id: 'mat-1', location_id: 'loc-1' },
  ];
  const errors = validateStockTakeForm(form);
  assert.match(errors['items.1.material_id'] ?? '', /重复/);
});

test('抽盘两个库位桶各一行是合法的（不算重复）', () => {
  const form = validPartialForm();
  form.items = [
    { key: 'line-1', material_id: 'mat-1', location_id: 'loc-1' },
    { key: 'line-2', material_id: 'mat-1', location_id: 'loc-2' },
  ];
  assert.deepEqual(validateStockTakeForm(form), {});
});

test('仓库 / 日期必填', () => {
  const form = emptyStockTakeForm(new Date('2026-11-04T02:00:00Z'));
  form.take_date = '';
  const errors = validateStockTakeForm(form);
  assert.ok(errors.warehouse_id);
  assert.ok(errors.take_date);
});

test('备注超 500 字报错（后端 MAX_REMARK_LEN）', () => {
  const form = emptyStockTakeForm(new Date('2026-11-04T02:00:00Z'));
  form.warehouse_id = 'wh-1';
  form.remark = 'x'.repeat(501);
  assert.ok(validateStockTakeForm(form).remark);
});

test('建单请求体：全盘 items 为空数组，抽盘只带 material_id / location_id', () => {
  const full = emptyStockTakeForm(new Date('2026-11-04T02:00:00Z'));
  full.warehouse_id = 'wh-1';
  full.remark = '  年底全盘  ';
  const fullPayload = buildStockTakePayload(full);
  assert.deepEqual(fullPayload.items, []);
  assert.equal(fullPayload.take_type, 'FULL');
  assert.equal(fullPayload.remark, '年底全盘', '备注要去空白');
  assert.equal('book_qty' in (fullPayload.items[0] ?? {}), false);

  const partial = validPartialForm();
  partial.items = [{ key: 'line-1', material_id: 'mat-1', location_id: 'loc-1' }];
  const partialPayload = buildStockTakePayload(partial);
  assert.deepEqual(partialPayload.items, [{ material_id: 'mat-1', location_id: 'loc-1' }]);
  // 账面数 / 单价 / 实盘数一律不该出现在请求体里（服务端快照）
  for (const key of ['book_qty', 'actual_qty', 'unit_cost', 'diff_qty']) {
    assert.equal(key in partialPayload.items[0]!, false, `${key} 不该发给后端`);
  }
  assert.equal(partialPayload.remark, null, '空备注发 null 而不是空串');
});

// ==================== 实盘数（改单） ====================

test('详情 → 实盘数编辑态：默认回显后端给的实盘数', () => {
  const detail = { items: [line('i1', 1, '100', '92'), line('i2', 2, '10', '10')] };
  assert.deepEqual(detailToActuals(detail), { i1: '92', i2: '10' });
});

test('实盘数：空 / 非数字 / 负数都拦，0 合法', () => {
  const items = [line('i1', 1, '100', '100'), line('i2', 2, '10', '10')];

  assert.equal(Object.keys(validateActuals(items, { i1: '100', i2: '' })).length, 1);
  assert.match(validateActuals(items, { i1: '100', i2: '' })['actuals.i2'] ?? '', /没填/);

  assert.match(validateActuals(items, { i1: 'abc', i2: '1' })['actuals.i1'] ?? '', /不是数字/);
  assert.match(validateActuals(items, { i1: '-1', i2: '1' })['actuals.i1'] ?? '', /不能为负/);

  assert.deepEqual(validateActuals(items, { i1: '0', i2: '0' }), {}, '0 是合法实盘数');
});

test('改单请求体：lines 用 item_id + actual_qty，日期与备注原样带', () => {
  const detail = {
    take_date: '2026-11-04',
    remark: '年底全盘',
    items: [line('i1', 1, '100', '92')],
  };
  const payload = buildActualPayload(detail, { i1: '92.5' });
  assert.equal(payload.take_date, '2026-11-04');
  assert.equal(payload.remark, '年底全盘');
  assert.deepEqual(payload.lines, [{ item_id: 'i1', actual_qty: '92.5' }]);
});

// ==================== 状态动作（只由 next_statuses 驱动） ====================

test('草稿态只出「提交审批 / 作废」两个按钮 —— 盘点没有「直接过账」', () => {
  // 后端 STOCK_TAKE_EDGES：DRAFT → PENDING_POST / CANCELLED（**没有 DRAFT → POSTED**）
  const actions = statusActions('DRAFT', ['PENDING_POST', 'CANCELLED']);
  assert.deepEqual(
    actions.map((item) => item.status),
    ['PENDING_POST', 'CANCELLED'],
  );
  assert.deepEqual(
    actions.map((item) => item.label),
    ['提交审批', '作废'],
  );
  assert.equal(actions.some((item) => item.status === 'POSTED'), false);
  assert.equal(actions[0]!.needConfirm, true, '提交审批要确认（会起流程）');
  assert.equal(actions[1]!.needReason, true, '作废要写原因');
});

test('待过账态出「过账 / 撤回 / 作废」三个按钮', () => {
  const actions = statusActions('PENDING_POST', ['POSTED', 'DRAFT', 'CANCELLED']);
  assert.deepEqual(
    actions.map((item) => item.label),
    ['过账', '撤回（回草稿）', '作废'],
  );
  assert.equal(actions[0]!.testid, 'btn-stocktake-post');
});

test('已过账态只剩作废（且必须写原因）', () => {
  const actions = statusActions('POSTED', ['CANCELLED']);
  assert.deepEqual(actions.map((item) => item.status), ['CANCELLED']);
  assert.equal(actions[0]!.needReason, true);
});

test('未知目标状态一律丢弃（不让页面渲出点不了 / 看不懂的按钮）', () => {
  assert.deepEqual(statusActions('DRAFT', ['ARCHIVED', 'PENDING_POST']).map((item) => item.status), ['PENDING_POST']);
  assert.deepEqual(statusActions('CANCELLED', []), []);
});

test('只有草稿能改 / 能删', () => {
  for (const status of ['DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED']) {
    assert.equal(canEdit({ status }), status === 'DRAFT');
    assert.equal(canDelete({ status }), status === 'DRAFT');
  }
});

// ==================== 作废 / 跳过审批 ====================

test('作废原因必填；已过账的报文要点名红字凭证', () => {
  assert.match(validateCancelReason('   ', 'DRAFT'), /请写一句作废原因/);
  assert.match(validateCancelReason('', 'POSTED'), /红字凭证/);
  assert.equal(validateCancelReason('仓库选错', 'DRAFT'), '');
  assert.match(validateCancelReason('x'.repeat(STOCK_TAKE_MAX_REASON + 1), 'POSTED'), /最长/);
  assert.deepEqual(buildCancelPayload('  仓库选错  '), { status: 'CANCELLED', reason: '仓库选错' });
});

test('66033 才算「没配已发布审批流程」', () => {
  assert.equal(isFlowNotConfigured({ code: CODE_FLOW_NOT_CONFIGURED }), true);
  assert.equal(isFlowNotConfigured({ code: 66001 }), false);
  assert.equal(isFlowNotConfigured(new Error('boom')), false);
  assert.equal(isFlowNotConfigured(null), false);
});

test('跳过审批理由 ≥ 10 字，请求体带 skip_approval + skip_reason', () => {
  assert.equal(MIN_SKIP_REASON_CHARS, 10);
  assert.equal(PERM_SKIP_APPROVAL, 'business:erp:skip-approval');
  assert.match(validateSkipReason('太短了'), /至少 10 字/);
  assert.equal(validateSkipReason('年度盘点点位已双人现场复核'), '');
  const payload = buildSkipSubmitPayload('  年度盘点点位已双人现场复核  ');
  assert.deepEqual(payload, {
    status: 'PENDING_POST',
    skip_approval: true,
    skip_reason: '年度盘点点位已双人现场复核',
  });
});

// ==================== 列表筛选 ====================

test('筛选 → 查询参数（空串一律不发）', () => {
  assert.deepEqual(buildStockTakeQuery(emptyStockTakeFilter(), 1), { page: 1, page_size: STOCK_TAKE_PAGE_SIZE });
  const query = buildStockTakeQuery(
    { keyword: ' ST-2026 ', status: 'POSTED', take_type: 'FULL', warehouse_id: 'wh-1' },
    2,
  );
  assert.deepEqual(query, {
    page: 2,
    page_size: STOCK_TAKE_PAGE_SIZE,
    keyword: 'ST-2026',
    status: 'POSTED',
    take_type: 'FULL',
    warehouse_id: 'wh-1',
  });
  assert.equal(isStockTakeFiltered(emptyStockTakeFilter()), false);
  assert.equal(isStockTakeFiltered({ ...emptyStockTakeFilter(), status: 'DRAFT' }), true);
});

// ==================== 展示口径 ====================

test('差异方向与符号：正数盘盈带 +，负数盘亏带 -，0 无符号', () => {
  assert.equal(diffDirection('3'), 'gain');
  assert.equal(diffDirection('-2'), 'loss');
  assert.equal(diffDirection('0'), 'zero');
  assert.equal(diffDirection(null), 'zero');
  assert.equal(formatDiff('3'), '+3');
  assert.equal(formatDiff('-2'), '-2');
  assert.equal(formatDiff('0'), '0');
  assert.equal(formatDiff('0.5000'), '+0.5');
});

test('状态历史只为真实发生过的里程碑打点', () => {
  const draft = statusHistory({
    created_at: '2026-11-04T01:00:00Z',
    status: 'DRAFT',
    posted_at: null,
    cancelled_at: null,
    cancel_reason: null,
  });
  assert.deepEqual(draft.map((item) => item.key), ['created']);

  const pending = statusHistory({
    created_at: '2026-11-04T01:00:00Z',
    status: 'PENDING_POST',
    posted_at: null,
    cancelled_at: null,
    cancel_reason: null,
  });
  assert.deepEqual(pending.map((item) => item.key), ['created', 'pending']);

  const posted = statusHistory({
    created_at: '2026-11-04T01:00:00Z',
    status: 'POSTED',
    posted_at: '2026-11-04T05:00:00Z',
    cancelled_at: null,
    cancel_reason: null,
  });
  assert.deepEqual(posted.map((item) => item.key), ['created', 'posted']);

  const cancelled = statusHistory({
    created_at: '2026-11-04T01:00:00Z',
    status: 'CANCELLED',
    posted_at: '2026-11-04T05:00:00Z',
    cancelled_at: '2026-11-04T06:00:00Z',
    cancel_reason: '盘错了仓库',
  });
  assert.deepEqual(cancelled.map((item) => item.key), ['created', 'posted', 'cancelled']);
  assert.equal(cancelled.at(-1)!.note, '盘错了仓库');
});

test('错误文案原样带上后端报文与错误码', () => {
  assert.equal(errorText({ code: 66033, message: '没有配置已发布的审批流程 `stock_take_post`' }), '没有配置已发布的审批流程 `stock_take_post`（错误码 66033）');
  assert.equal(errorText({ message: '抽盘明细里有重复行' }), '抽盘明细里有重复行');
  assert.equal(errorText('裸字符串'), '裸字符串');
  assert.match(errorText(null), /接口没有返回说明/);
});

// ==================== 盘盈 / 盘亏口径（草稿 ≠ 后端合计） ====================

test('aggregateDiff：按明细 diff_qty 汇总盘盈 / 盘亏数量', () => {
  const items = [line('i1', 1, '100', '92'), line('i2', 2, '10', '18'), line('i3', 3, '5', '5')];
  const agg = aggregateDiff(items);
  assert.deepEqual(agg, { gainQty: 8, lossQty: 8, diffLines: 2 });
  assert.deepEqual(aggregateDiff([]), { gainQty: 0, lossQty: 0, diffLines: 0 });
});

test('gainLossDisplay：草稿按明细汇总（标 fromBackend=false），已过账用后端合计', () => {
  const items = [line('i1', 1, '0', '5')];
  const draft = gainLossDisplay({
    status: 'DRAFT',
    total_gain_qty: '0.0000',
    total_loss_qty: '0.0000',
    items,
  });
  // 后端草稿阶段只重算账面 / 实盘合计，盘盈/盘亏恒 0 —— 直接渲染会误导用户
  assert.deepEqual(draft, { gain: '5', loss: '0', fromBackend: false });

  const posted = gainLossDisplay({
    status: 'POSTED',
    total_gain_qty: '5.0000',
    total_loss_qty: '0.0000',
    items,
  });
  assert.deepEqual(posted, { gain: '5', loss: '0', fromBackend: true });
});

// ==================== 前端侧镜像 n08：路由 + 视图文件真的存在 ====================
const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = resolve(here, '../..');

/** 与 `backend/src/system/menu/tests.rs::n08` 同一套解析：`path: 'xxx'` → `/xxx` */
function registeredPaths(): string[] {
  const source = readFileSync(resolve(frontendRoot, 'src/router/index.ts'), 'utf-8');
  const out: string[] = [];
  for (const line of source.split('\n')) {
    const idx = line.indexOf("path: '");
    if (idx < 0) {
      continue;
    }
    const rest = line.slice(idx + "path: '".length);
    const end = rest.indexOf("'");
    if (end < 0) {
      continue;
    }
    const raw = rest.slice(0, end);
    if (!raw || raw === '/' || raw.startsWith(':')) {
      continue;
    }
    out.push(`/${raw.replace(/^\/+/, '')}`);
  }
  return out;
}

test('V0211 的菜单 path 已在 router/index.ts 注册（n08 的前端侧镜像）', () => {
  const registered = registeredPaths();
  assert.ok(registered.length > 50, '路由表解析结果不该这么少，解析逻辑可能失效');
  assert.ok(
    registered.includes('/business/erp/stock-take'),
    `router/index.ts 里找不到 /business/erp/stock-take，点菜单会 404。已注册样例：${registered.slice(0, 5).join(', ')}`,
  );
});

test('菜单 component 指向的视图文件真的存在', () => {
  // V0211 种子：component = 'business/erp/stock-take/index'
  const view = resolve(frontendRoot, 'src/views/business/erp/stock-take/index.vue');
  assert.equal(existsSync(view), true, `${view} 不存在`);
  const source = readFileSync(view, 'utf-8');
  assert.match(source, /<template>/, '视图文件必须是可渲染的 SFC');
  // 页面必须真的接后端 6 个权限码里的关键几个（不是空壳）
  for (const code of [
    'business:erp:stock-take:create',
    'business:erp:stock-take:update',
    'business:erp:stock-take:delete',
    'business:erp:stock-take:status',
  ]) {
    assert.ok(source.includes(code), `页面缺少权限码 ${code}`);
  }
  assert.match(source, /api\/business\/erp\/stock-take/, '页面必须走盘点单 API 层');
});

test('详情「差异凭证 / 红冲凭证」必须显示凭证号（PZ-… / PZR-…），不许拿 UUID 截断顶替', () => {
  const view = resolve(frontendRoot, 'src/views/business/erp/stock-take/index.vue');
  const source = readFileSync(view, 'utf-8');
  // 差异凭证块：主值必须是 voucher_no，点击跳凭证列表核账（凭证列表按 voucher_no 检索）
  const voucherBlock = source.slice(
    source.indexOf('data-testid="stocktake-detail-voucher"') - 400,
    source.indexOf('data-testid="stocktake-detail-voucher"') + 900,
  );
  assert.match(voucherBlock, /detail\.voucher_no/, '差异凭证必须渲染 voucher_no');
  assert.ok(
    !/detail\.voucher_id \? shortId\(detail\.voucher_id\)/.test(source),
    '不能再把 shortId(voucher_id)（UUID 截断）当凭证号显示',
  );
  // 红冲凭证一并处理（作废后才有，别只修一半）
  const reverseBlock = source.slice(
    source.indexOf('data-testid="stocktake-detail-reverse-voucher"') - 400,
    source.indexOf('data-testid="stocktake-detail-reverse-voucher"') + 900,
  );
  assert.match(reverseBlock, /detail\.reverse_voucher_no/, '红冲凭证必须渲染 reverse_voucher_no');
  // 点号跳转：凭证页支持 ?voucher_no= 深链预填筛选
  assert.match(source, /\/business\/erp\/voucher/, '凭证号应可跳凭证列表核账');
  assert.match(source, /voucher_no: voucherNo/, '跳转必须带 voucher_no 过滤参数');
});

/** 让 TS 认为 StockTakeDetail 被用过（详情形状在类型层面校验） */
test('详情契约：单头扁平 + items + next_statuses', () => {
  const detail: Pick<StockTakeDetail, 'items' | 'next_statuses'> = {
    items: [line('i1', 1, '100', '92')],
    next_statuses: ['POSTED', 'DRAFT', 'CANCELLED'],
  };
  assert.equal(detail.items.length, 1);
  assert.equal(statusActions('PENDING_POST', detail.next_statuses).length, 3);
  assert.ok(STOCK_TAKE_MAX_LINES >= detail.items.length);
});
