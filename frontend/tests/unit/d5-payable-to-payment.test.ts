/**
 * D5「应付单 → 发起付款」前端纯逻辑单测。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 为什么测 `.ts` 不测 `.vue`
 *
 * 卡片 F2 / O1 / L9 的三条硬要求都落在**纯函数**上：
 *
 * | 卡面 | 断言点 | 函数 |
 * |---|---|---|
 * | F2 带「未付」不带「总额」 | `payPrefillAmount` 必须等于 `default_amount`（= 未付） | `payPrefillAmount` |
 * | O1 默认值 = 未付 | 同上 + 应付 500 / 已付 200 时预填 300，不是 500 | `payPrefillAmount` |
 * | L9 已结清 / 已作废不显示按钮 | `can_pay = false` ⇒ 按钮 false（**不是置灰**） | `canStartPay` |
 *
 * 金额口径照后端抄：`payable_payment.rs::PaymentInfoPayable`
 * （`default_amount` 在 `can_pay = false` 时是 0、`unpaid_amount = amount - paid_amount`）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  canStartPay,
  payableDocPath,
  payPrefillAmount,
  sourceDocPath,
  voucherDocPath,
  type PayableInfoLike,
} from '@/views/business/erp/payable/payable-logic';

/** 造一份可付信息单头（默认 OPEN、能付、未付 = 应付） */
function info(overrides: Partial<PayableInfoLike> = {}): PayableInfoLike {
  return {
    can_pay: true,
    blocked_reason: null,
    amount: '226.0000',
    unpaid_amount: '226.0000',
    default_amount: '226.0000',
    ...overrides,
  };
}

// ---------- F2 / O1：预填金额取未付 ----------

test('d5 预填金额 = 后端 default_amount（部分已付时是 300 而不是 500）', () => {
  const partial = info({
    amount: '500.0000',
    unpaid_amount: '300.0000',
    default_amount: '300.0000',
  });
  assert.equal(payPrefillAmount(partial), 300);
  assert.notEqual(payPrefillAmount(partial), 500, 'F2：绝不能带出应付总额');
});

test('d5 预填金额：整数分不丢精度（126.35 → 126.35）', () => {
  const row = info({
    amount: '226.35',
    unpaid_amount: '126.35',
    default_amount: '126.35',
  });
  assert.equal(payPrefillAmount(row), 126.35);
});

test('d5 预填金额：后端没给 default_amount 时退回未付（灰度期的老响应）', () => {
  const legacy = {
    can_pay: true,
    blocked_reason: null,
    amount: '500',
    unpaid_amount: '300',
  } as unknown as PayableInfoLike;
  assert.equal(payPrefillAmount(legacy), 300);
});

test('d5 预填金额：不能付款时是 0（后端把 default_amount 归 0）', () => {
  const settled = info({
    can_pay: false,
    blocked_reason: '应付单 AP-1 已结清（未付金额 0.0000），无需再付款',
    unpaid_amount: '0.0000',
    default_amount: '0.0000',
  });
  assert.equal(payPrefillAmount(settled), 0);
  assert.equal(payPrefillAmount(null), 0);
});

// ---------- L9：按钮显不显示 ----------

test('d5 按钮：能付 + 有付款权限 → 显示', () => {
  assert.equal(canStartPay(info(), true), true);
});

test('d5 按钮：已结清 → 不显示（不是置灰）', () => {
  const settled = info({
    can_pay: false,
    blocked_reason: '应付单 AP-1 已结清（未付金额 0.0000），无需再付款',
    unpaid_amount: '0',
    default_amount: '0',
  });
  assert.equal(canStartPay(settled, true), false);
});

test('d5 按钮：已作废 → 不显示', () => {
  const cancelled = info({
    can_pay: false,
    blocked_reason: '应付单 AP-1 已作废（CANCELLED），不能再付款',
    default_amount: '0',
  });
  assert.equal(canStartPay(cancelled, true), false);
});

test('d5 按钮：审批中（can_pay=false）→ 不显示，且原因要能透出给用户', () => {
  const pending = info({
    can_pay: false,
    blocked_reason: '该应付单已有一笔付款在审批中（PM-20261029-000001，金额 100.0000）',
    default_amount: '0',
  });
  assert.equal(canStartPay(pending, true), false);
  assert.match(String(pending.blocked_reason), /审批中/);
});

test('d5 按钮：没有 business:erp:payable:pay 权限 → 不显示（后端仍会 403）', () => {
  assert.equal(canStartPay(info(), false), false);
});

test('d5 按钮：可付信息还没拉到（null）→ 不显示，避免闪现一个点不动的按钮', () => {
  assert.equal(canStartPay(null, true), false);
});

// ---------- F3：三段可追溯链路 ----------

test('d5 追溯：来源收货单 → 收货单页深链（带 in_id）', () => {
  assert.equal(
    sourceDocPath('STOCK_IN', '7f3a1b2c-0000-4000-8000-000000000001'),
    '/business/erp/stock-in?in_id=7f3a1b2c-0000-4000-8000-000000000001',
  );
});

test('d5 追溯：非收货来源 / 没有 id → 不给假链接', () => {
  assert.equal(sourceDocPath('MANUAL', 'x'), null);
  assert.equal(sourceDocPath('STOCK_IN', ''), null);
  assert.equal(sourceDocPath('STOCK_IN', null), null);
});

test('d5 追溯：付款凭证号 → 凭证页深链；空号 → null', () => {
  assert.equal(voucherDocPath('PZ-20261029-000001'), '/business/erp/voucher?voucher_no=PZ-20261029-000001');
  assert.equal(voucherDocPath('  '), null);
  assert.equal(voucherDocPath(null), null);
});

test('d5 追溯：应付单号 → 应付页深链（付款记录点回来）', () => {
  assert.equal(payableDocPath('AP-20261031-000001'), '/business/erp/payable?payable_no=AP-20261031-000001');
  assert.equal(payableDocPath(''), null);
  // 单号里出现特殊字符要转义，不能拼出坏链接
  assert.equal(payableDocPath('AP 1&2'), '/business/erp/payable?payable_no=AP%201%262');
});
