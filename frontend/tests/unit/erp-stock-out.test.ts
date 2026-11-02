/**
 * 出库单页纯逻辑单测（DC-13 前端收尾）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零依赖、无 DOM）。
 *
 * 测 `.ts` 不测 `.vue` 的原因同 `stockin-logic` / `transfer-logic`：用例跑在没有 DOM 的
 * node 里，而「按钮该不该出、作废要不要拦、请求体发什么」全在逻辑层；
 * `.vue` 只负责渲染，这些一旦漂移 `vue-tsc` 抓不到。
 *
 * 断言口径照后端抄（`backend/src/business/erp/stockout_{base,dto}.rs`）：
 * - 类型四值 `OUT_TYPES`、状态四值 `STATUS_*`；
 * - `SCRAP` / `LOSS` 必须填事由（凭证要挂 1901）；
 * - 明细 `StockOutItemReq` **没有** 单价 / 金额 / 单位；
 * - 动作目标 `TRANSITION_TARGETS = ["POSTED","PENDING_POST","DRAFT","CANCELLED"]`。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  OUT_TYPE_OPTIONS,
  REQUIRES_REASON_TYPES,
  STOCK_OUT_MAX_LINES,
  STOCK_OUT_PAGE_SIZE,
  STOCK_OUT_STATUS_FILTER_OPTIONS,
  buildCancelPayload,
  buildStockOutPayload,
  buildStockOutQuery,
  canDelete,
  canEdit,
  detailToForm,
  emptyStockOutFilter,
  emptyStockOutForm,
  errorText,
  formatQty,
  isCancelBlockedError,
  isNegativeStockError,
  isStockOutFiltered,
  outTypeLabel,
  requiresReason,
  statusActions,
  statusHistory,
  stockOutStatusLabel,
  stockOutStatusTheme,
  validateCancelReason,
  validateStockOutForm,
  type StockOutFormState,
} from '../../src/views/business/erp/stock-out/stock-out-logic.ts';

/** 一个填得完全合法的领用单（各用例在此基础上只改一处） */
function validForm(): StockOutFormState {
  const form = emptyStockOutForm(new Date('2026-11-03T02:00:00Z'));
  form.warehouse_id = 'wh-1';
  form.items = [{ key: 'line-1', material_id: 'mat-1', qty: 30 }];
  return form;
}

// ==================== 枚举 / 译名 ====================

test('出库类型四值与中文名逐字对齐后端 OUT_TYPES', () => {
  assert.deepEqual(
    OUT_TYPE_OPTIONS.map((item) => item.value),
    ['ISSUE', 'SCRAP', 'LOSS', 'OTHER'],
  );
  assert.deepEqual(
    OUT_TYPE_OPTIONS.map((item) => item.label),
    ['领用', '报废', '盘亏', '其他'],
  );
  // 后端 out_type_label 的译文（列表 / 详情都直接显示后端给的那份）
  assert.equal(outTypeLabel('ISSUE'), '领用');
  assert.equal(outTypeLabel('SCRAP'), '报废');
  assert.equal(outTypeLabel('LOSS'), '盘亏');
  assert.equal(outTypeLabel('OTHER'), '其他');
  // 未知值不吞掉，原样暴露出来（便于发现后端加值前端没跟上）
  assert.equal(outTypeLabel('BOGUS'), 'BOGUS');
});

test('只有报废 / 盘亏必须填事由（要挂 1901）', () => {
  assert.deepEqual([...REQUIRES_REASON_TYPES], ['SCRAP', 'LOSS']);
  assert.equal(requiresReason('SCRAP'), true);
  assert.equal(requiresReason('LOSS'), true);
  assert.equal(requiresReason('ISSUE'), false);
  assert.equal(requiresReason('OTHER'), false);
});

test('状态四值的中文名与标签主题', () => {
  assert.deepEqual(
    STOCK_OUT_STATUS_FILTER_OPTIONS.map((item) => item.value),
    ['DRAFT', 'PENDING_POST', 'POSTED', 'CANCELLED'],
  );
  assert.equal(stockOutStatusLabel('DRAFT'), '草稿');
  assert.equal(stockOutStatusLabel('PENDING_POST'), '待过账');
  assert.equal(stockOutStatusLabel('POSTED'), '已过账');
  assert.equal(stockOutStatusLabel('CANCELLED'), '已作废');
  assert.equal(stockOutStatusTheme('PENDING_POST'), 'warning');
  assert.equal(stockOutStatusTheme('POSTED'), 'success');
  assert.equal(stockOutStatusTheme('CANCELLED'), 'danger');
});

// ==================== 动作按钮只由 next_statuses 驱动 ====================

test('按钮 = next_statuses 的投影：只喂两个目标就只出两个按钮', () => {
  const actions = statusActions('DRAFT', ['POSTED', 'CANCELLED']);
  assert.deepEqual(
    actions.map((item) => item.status),
    ['POSTED', 'CANCELLED'],
  );
  assert.deepEqual(
    actions.map((item) => item.label),
    ['过账', '作废'],
  );
  assert.equal(actions[0].theme, 'primary');
  assert.equal(actions[1].theme, 'danger');
  // 作废必须先填原因，过账不用
  assert.equal(actions[0].needReason, false);
  assert.equal(actions[1].needReason, true);
});

test('next_statuses 为空 → 一个按钮都不出（已作废单据再也不能流转）', () => {
  assert.deepEqual(statusActions('CANCELLED', []), []);
  // 后端哪天收回 DRAFT → POSTED 直过这条边，前端自动跟着消失
  assert.deepEqual(
    statusActions('DRAFT', ['PENDING_POST', 'CANCELLED']).map((item) => item.status),
    ['PENDING_POST', 'CANCELLED'],
  );
});

test('目标态文案看「当前状态」：PENDING_POST → DRAFT 是撤回，其它是退回', () => {
  assert.equal(statusActions('PENDING_POST', ['DRAFT'])[0].label, '撤回（回草稿）');
  assert.equal(statusActions('DRAFT', ['DRAFT'])[0].label, '退回草稿');
});

test('next_statuses 里的未知目标被丢弃，重复目标只留一个', () => {
  const actions = statusActions('DRAFT', ['POSTED', 'ARCHIVED', 'POSTED', '']);
  assert.deepEqual(
    actions.map((item) => item.status),
    ['POSTED'],
  );
});

// ==================== 表单校验 ====================

test('空领用单：缺仓库 / 缺物料 / 数量为 0 三处同时报错', () => {
  const form = emptyStockOutForm(new Date('2026-11-03T02:00:00Z'));
  const errors = validateStockOutForm(form);
  assert.equal(errors.warehouse_id, '请选择出库仓库');
  assert.ok(errors['items.0.material_id']);
  assert.ok(errors['items.0.qty']);
  // 类型 / 日期有默认值，不该报
  assert.equal(errors.out_type, undefined);
  assert.equal(errors.out_date, undefined);
  assert.equal(errors.reason, undefined);
});

test('报废 / 盘亏没写事由被拦住，写了就放行', () => {
  const scrap = validForm();
  scrap.out_type = 'SCRAP';
  assert.match(validateStockOutForm(scrap).reason ?? '', /必须填事由/);

  scrap.reason = '   ';
  assert.match(validateStockOutForm(scrap).reason ?? '', /必须填事由/);

  scrap.reason = '锈蚀报废';
  assert.equal(validateStockOutForm(scrap).reason, undefined);

  // 领用不强制（但后端字段本身可空）
  const issue = validForm();
  assert.equal(validateStockOutForm(issue).reason, undefined);
});

test('一物一码物料被前端提前拦下（后端会 400 指路序列号管理）', () => {
  const form = validForm();
  const errors = validateStockOutForm(form, ['mat-1']);
  assert.match(errors['items.0.material_id'] ?? '', /一物一码物料不能走出库单/);
  // 不在名单里的物料照常通过
  assert.deepEqual(validateStockOutForm(validForm(), ['mat-other']), {});
});

test('数量必须 > 0：0 与负数都拦', () => {
  const zero = validForm();
  zero.items[0].qty = 0;
  assert.match(validateStockOutForm(zero)['items.0.qty'] ?? '', /必须 > 0/);

  const negative = validForm();
  negative.items[0].qty = -5;
  assert.match(validateStockOutForm(negative)['items.0.qty'] ?? '', /必须 > 0/);

  const missing = validForm();
  missing.items[0].qty = undefined;
  assert.match(validateStockOutForm(missing)['items.0.qty'] ?? '', /必须 > 0/);
});

test('明细为空 / 超上限都被拦，且错误挂在 items 上', () => {
  const empty = validForm();
  empty.items = [];
  assert.equal(validateStockOutForm(empty).items, '至少要有一行明细');

  const tooMany = validForm();
  tooMany.items = Array.from({ length: STOCK_OUT_MAX_LINES + 1 }, (_, index) => ({
    key: `line-${index}`,
    material_id: 'mat-1',
    qty: 1,
  }));
  assert.match(validateStockOutForm(tooMany).items ?? '', /明细最多/);
});

// ==================== 请求体 ====================

test('建单 body 只带后端认的字段：明细没有单价 / 金额 / 单位', () => {
  const form = validForm();
  form.out_type = 'LOSS';
  form.reason = '  盘亏核销  ';
  form.remark = '   ';
  form.items[0].location_id = 'loc-row-1';
  const payload = buildStockOutPayload(form);

  assert.equal(payload.out_type, 'LOSS');
  assert.equal(payload.warehouse_id, 'wh-1');
  assert.equal(payload.out_date, '2026-11-03');
  assert.equal(payload.reason, '盘亏核销'); // 去空白
  assert.equal(payload.remark, null); // 空串 → null（不留 "" 让后端猜）
  assert.deepEqual(Object.keys(payload.items[0]).sort(), ['location_id', 'material_id', 'qty']);
  assert.equal(payload.items[0].qty, '30'); // Decimal 用字符串
  assert.equal(payload.items[0].location_id, 'loc-row-1');
  // 显式断言「三个不该出现的字段」
  for (const forbidden of ['unit_cost', 'amount', 'unit_id', 'price', 'price_excl']) {
    assert.equal(forbidden in payload.items[0], false, `明细不该带 ${forbidden}`);
  }
});

test('数量转字符串时去掉浮点尾巴', () => {
  const form = validForm();
  form.items[0].qty = 1.0000001;
  assert.equal(buildStockOutPayload(form).items[0].qty, '1');
  assert.equal(formatQty('30.0000'), '30');
});

test('筛选条件：空串一律不发，日期区间拆成 date_from / date_to', () => {
  assert.deepEqual(buildStockOutQuery(emptyStockOutFilter(), 1), { page: 1, page_size: STOCK_OUT_PAGE_SIZE });

  const query = buildStockOutQuery(
    {
      keyword: '  OUT-2026  ',
      status: 'POSTED',
      out_type: 'ISSUE',
      warehouse_id: 'wh-1',
      date_range: ['2026-11-01', '2026-11-03'],
    },
    2,
  );
  assert.deepEqual(query, {
    page: 2,
    page_size: STOCK_OUT_PAGE_SIZE,
    keyword: 'OUT-2026',
    status: 'POSTED',
    out_type: 'ISSUE',
    warehouse_id: 'wh-1',
    date_from: '2026-11-01',
    date_to: '2026-11-03',
  });
});

test('isStockOutFiltered 只认真正填了的条件', () => {
  assert.equal(isStockOutFiltered(emptyStockOutFilter()), false);
  assert.equal(
    isStockOutFiltered({ ...emptyStockOutFilter(), keyword: '   ' }),
    false,
    '纯空白不算筛选',
  );
  assert.equal(isStockOutFiltered({ ...emptyStockOutFilter(), status: 'DRAFT' }), true);
  assert.equal(isStockOutFiltered({ ...emptyStockOutFilter(), date_range: ['2026-11-01'] }), true);
});

// ==================== 状态历史 ====================

test('状态历史只为真实发生过的里程碑打点', () => {
  const draft = statusHistory({
    created_at: '2026-11-03T02:00:00Z',
    status: 'DRAFT',
    posted_at: null,
    cancelled_at: null,
    cancel_reason: null,
  });
  assert.deepEqual(
    draft.map((item) => item.key),
    ['created'],
  );
  assert.equal(draft[0].at, '2026-11-03T02:00:00Z');

  const posted = statusHistory({
    created_at: '2026-11-03T02:00:00Z',
    status: 'POSTED',
    posted_at: '2026-11-03T03:00:00Z',
    cancelled_at: null,
    cancel_reason: null,
  });
  assert.deepEqual(
    posted.map((item) => item.key),
    ['created', 'posted'],
  );

  const cancelled = statusHistory({
    created_at: '2026-11-03T02:00:00Z',
    status: 'CANCELLED',
    posted_at: '2026-11-03T03:00:00Z',
    cancelled_at: '2026-11-03T04:00:00Z',
    cancel_reason: '领错料，退回',
  });
  assert.deepEqual(
    cancelled.map((item) => item.key),
    ['created', 'posted', 'cancelled'],
  );
  assert.equal(cancelled[2].note, '领错料，退回');
});

test('待过账的历史不编时间（后端没下发提交时间）', () => {
  const pending = statusHistory({
    created_at: '2026-11-03T02:00:00Z',
    status: 'PENDING_POST',
    posted_at: null,
    cancelled_at: null,
    cancel_reason: null,
  });
  assert.deepEqual(
    pending.map((item) => item.key),
    ['created', 'pending'],
  );
  assert.equal(pending[1].at, null);
  assert.match(pending[1].note, /等待他人过账/);
});

// ==================== 作废 / 编辑权限 / 错误展示 ====================

test('作废原因必填，且请求体把原因带上', () => {
  assert.match(validateCancelReason('   '), /作废必须填原因/);
  assert.match(validateCancelReason('x'.repeat(256)), /最长/);
  assert.equal(validateCancelReason('领错料，退回'), '');
  assert.deepEqual(buildCancelPayload('  领错料，退回  '), { status: 'CANCELLED', reason: '领错料，退回' });
});

test('只有草稿能编辑 / 删除', () => {
  assert.equal(canEdit({ status: 'DRAFT' }), true);
  assert.equal(canEdit({ status: 'PENDING_POST' }), false);
  assert.equal(canEdit({ status: 'POSTED' }), false);
  assert.equal(canDelete({ status: 'DRAFT' }), true);
  assert.equal(canDelete({ status: 'CANCELLED' }), false);
});

test('错误文案原样带后端 msg（负库存的「可用量」不能被吞掉）', () => {
  const negative = {
    code: 66034,
    message: '负库存拦截：物料 M-001（螺栓）在仓库 WH-01 / 库位 A-01 可用量 3，本次出库 5',
  };
  const text = errorText(negative);
  assert.match(text, /可用量 3/);
  assert.match(text, /66034/);
  assert.equal(isNegativeStockError(negative), true);
  assert.equal(isCancelBlockedError(negative), false);

  const blocked = { code: 66035, message: '作废失败：出库流水缺行，拒绝按现价估算' };
  assert.equal(isCancelBlockedError(blocked), true);
  assert.equal(isNegativeStockError(blocked), false);

  // 兜底：没有 message 时也要给一句人话，而不是空字符串
  assert.match(errorText({}), /操作失败/);
  assert.match(errorText('网络错误'), /网络错误/);
});

test('详情 → 编辑表单：数量转成数字，库位缺省为 null', () => {
  const form = detailToForm({
    id: 'so-1',
    out_no: 'OUT-20261103-000001',
    out_type: 'ISSUE',
    out_type_label: '领用',
    warehouse_id: 'wh-1',
    out_date: '2026-11-03',
    status: 'DRAFT',
    status_label: '草稿',
    total_qty: '30.0000',
    total_amount: '0.0000',
    voucher_id: null,
    reason: '车间领用',
    remark: null,
    created_at: '2026-11-03T02:00:00Z',
    create_by: null,
    items: [
      {
        id: 'item-1',
        line_no: 1,
        material_id: 'mat-1',
        material_code: 'M-001',
        material_name: '螺栓',
        unit_id: 'unit-1',
        qty: '30.0000',
        unit_cost: '0.0000',
        amount: '0.0000',
        location_id: null,
        lot_no: null,
        remark: null,
      },
    ],
    posted_at: null,
    cancelled_at: null,
    cancel_reason: null,
    next_statuses: ['POSTED', 'PENDING_POST', 'CANCELLED'],
    legal_edges: [],
  });

  assert.equal(form.id, 'so-1');
  assert.equal(form.out_type, 'ISSUE');
  assert.equal(form.reason, '车间领用');
  assert.equal(form.items.length, 1);
  assert.equal(form.items[0].qty, 30);
  assert.equal(typeof form.items[0].qty, 'number');
});

// ==================== 源码即契约：路径必须带 /api ====================

test('API 层 6 个接口都写全了 /api 前缀（漏了会命中 SPA fallback 返回 HTML）', () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const source = readFileSync(
    resolve(here, '../../src/api/business/erp/stock-out.ts'),
    'utf8',
  );
  assert.match(source, /const BASE = '\/api\/business\/erp'/);
  for (const endpoint of [
    '`${BASE}/stock-out/page`',
    '`${BASE}/stock-out/${id}`',
    '`${BASE}/stock-out`',
    '`${BASE}/stock-out/${id}/status`',
  ]) {
    assert.ok(source.includes(endpoint), `缺少接口路径 ${endpoint}`);
  }
  // 四个写接口 + 两个读接口都在
  for (const fn of ['fetchStockOutPage', 'fetchStockOutDetail', 'createStockOut', 'updateStockOut', 'deleteStockOut', 'updateStockOutStatus']) {
    assert.ok(source.includes(`export function ${fn}`), `缺少导出函数 ${fn}`);
  }
});
