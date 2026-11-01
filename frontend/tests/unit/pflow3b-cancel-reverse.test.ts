/**
 * P-FLOW-3B 前端配套单测：作废/红冲「审批中」徽标 + 撤回按钮 + **送审提示文案**。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 为什么这几条必须测
 *
 * 这一批改动的坏味道都在「文案 / 可见性」里，`vue-tsc` 抓不到、e2e 又只看得见屏幕：
 *
 * | 写错了会怎样 | 钉它的用例 |
 * |---|---|
 * | 送审后提示「已作废 / 已红冲」 → 用户以为钱已经退了、账已经冲了 | ④ ⑥ 断言 `已作废` / `已红冲` **不出现** |
 * | 作废审批中还显示「付款」「作废」 → 点了必被后端 409 | ② ③ |
 * | 红冲审批中还显示「红冲」 → 点了必 409「已在红冲审批中」 | ⑦ |
 * | 徽标文案与后端 `status_label` 漂移 | ① 同时断言前端文案与后端源码字面量 |
 * | 后端补了 `pending_reverse_instance_id` 前端没跟上 → 永远显示不出徽标 | ⑧ 字段必填 + 兜底已删 |
 * | 有人把「审批实例分页」那层兜底又加回来（多打请求 + 受可见性限制） | ⑧ 源码扫描 |
 * | 抽屉里作废进度 / 原因只在「送审那一刻」有，重开抽屉就没了 | ⑨ `approvalTargetOf` / `cancelReasonText` |
 *
 * 断言口径直接抄后端（`readFileSync` 读源码比对，后端改一个字这里先红）：
 * - `backend/src/business/erp/payable.rs` → `STATUS_PENDING_CANCEL = "PENDING_CANCEL"`；
 * - `backend/src/business/erp/payable_query.rs` → `"作废审批中"`；
 * - `backend/src/business/erp/voucher_write.rs` → `REVERSE_BIZ_TYPE = "VOUCHER_REVERSE"` /
 *   `OUTCOME_REVERSE_PENDING = "PENDING_APPROVAL"`；
 * - `backend/src/business/erp/payable_payment.rs` → `OUTCOME_CANCEL_PENDING = "PENDING_APPROVAL"`。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  PAYABLE_STATUS_FILTER_OPTIONS,
  PAYABLE_STATUS_PENDING_CANCEL,
  approvalTargetOf,
  canCancelPayable,
  canPayPayable,
  canStartPay,
  canWithdrawCancel,
  cancelReasonText,
  isPendingCancel,
  payableStatusLabel,
  payableStatusTheme,
  withdrawCancelBlockedReason,
} from '@/views/business/erp/payable/payable-logic';
import {
  VOUCHER_PENDING_REVERSE_LABEL,
  canReverseVoucher,
  canWithdrawReverse,
  isPendingReverse,
  withdrawReverseBlockedReason,
} from '@/views/business/erp/voucher/voucher-logic';
import {
  OUTCOME_CANCELLED,
  OUTCOME_PENDING_APPROVAL,
  OUTCOME_REVERSED,
  PENDING_APPROVAL_HINT,
  isPendingApproval,
  pendingBadgeLabel,
  submitFeedback,
  withdrawFeedback,
} from '@/views/business/erp/shared/approval-flow';

// ==================== fixture ====================

/** 后端源码（相对本仓根：`frontend/tests/unit` → `../../../backend`）。 */
function backendSource(relative: string): string {
  return readFileSync(new URL(`../../../backend/src/business/erp/${relative}`, import.meta.url), 'utf8');
}

/** 应付单（只映出判据用得到的两个字段）。 */
function payable(status: string, paid: number | string = 0): { status: string; paid_amount: number | string } {
  return { status, paid_amount: paid };
}

/** 可付信息（后端 `payable.payment-info` 的 payable 节点，只挑用得到的）。 */
function payInfo(canPay: boolean) {
  return {
    can_pay: canPay,
    blocked_reason: canPay ? null : '已作废',
    default_amount: 226 as number | string,
    unpaid_amount: 226 as number | string,
    amount: 226 as number | string,
  };
}

/** 凭证行（判据只吃这几个字段）。 */
function voucher(overrides: Record<string, unknown> = {}) {
  return {
    status: 'POSTED',
    is_red: false as boolean | null,
    reverse_of: null as string | null,
    reversed_by: null as string | null,
    reversed_by_no: null as string | null,
    pending_reverse_instance_id: null as string | null,
    ...overrides,
  };
}

// ==================== ① 状态映射（前后端字面量互相对照） ====================

test('① 应付 PENDING_CANCEL → 「作废审批中」+ warning，文案与后端 status_label 逐字一致', () => {
  // 后端常量 / 标签（源码即契约）
  const payableSrc = backendSource('payable.rs');
  assert.ok(
    payableSrc.includes('pub const STATUS_PENDING_CANCEL: &str = "PENDING_CANCEL";'),
    '后端 payable.rs 里没有 STATUS_PENDING_CANCEL = "PENDING_CANCEL"',
  );
  const querySrc = backendSource('payable_query.rs');
  assert.ok(
    querySrc.includes('s if s == payable::STATUS_PENDING_CANCEL => "作废审批中"'),
    '后端 payable_query.rs 的状态文案不是「作废审批中」，前端文案要跟着改',
  );

  // 前端字面量与后端一致
  assert.equal(PAYABLE_STATUS_PENDING_CANCEL, 'PENDING_CANCEL');
  assert.equal(payableStatusLabel('PENDING_CANCEL'), '作废审批中');
  assert.equal(payableStatusLabel(PAYABLE_STATUS_PENDING_CANCEL), '作废审批中');

  // 橙色 = 「还没定」；不能给灰（灰一眼看过去像已作废）也不能给绿（绿像已结清）
  assert.equal(payableStatusTheme('PENDING_CANCEL'), 'warning');

  // 老 4 态不许被这次改动带偏
  assert.equal(payableStatusLabel('OPEN'), '未付');
  assert.equal(payableStatusLabel('PARTIAL_PAID'), '部分付款');
  assert.equal(payableStatusLabel('SETTLED'), '已结清');
  assert.equal(payableStatusLabel('CANCELLED'), '已作废');
  assert.equal(payableStatusTheme('CANCELLED'), 'default');
});

test('① 状态下拉能筛出「作废审批中」（共 6 项，值顺序稳定）', () => {
  assert.deepEqual(
    PAYABLE_STATUS_FILTER_OPTIONS.map((item) => item.value),
    ['', 'OPEN', 'PARTIAL_PAID', 'SETTLED', 'PENDING_CANCEL', 'CANCELLED'],
  );
  assert.equal(
    PAYABLE_STATUS_FILTER_OPTIONS.find((item) => item.value === 'PENDING_CANCEL')?.label,
    '作废审批中',
  );
});

// ==================== ② 按钮可见性（列表行） ====================

test('② 作废审批中：「付款」「作废」都不可点，「撤回作废」可点（且有权限时）', () => {
  const pending = payable('PENDING_CANCEL', 0);

  assert.equal(isPendingCancel(pending), true);
  assert.equal(canPayPayable(pending), false, '作废审批中不能再付款（后端会 409）');
  assert.equal(canCancelPayable(pending), false, '作废审批中不能再作废（后端会 409）');
  assert.equal(canWithdrawCancel(pending, true), true);
  assert.equal(canWithdrawCancel(pending, false), false, '没撤回权限时不给撤回入口');

  // 老状态矩阵原样不变
  assert.equal(canPayPayable(payable('OPEN', 0)), true);
  assert.equal(canPayPayable(payable('PARTIAL_PAID', 100)), true);
  assert.equal(canPayPayable(payable('SETTLED', 226)), false);
  assert.equal(canCancelPayable(payable('OPEN', 0)), true);
  assert.equal(canCancelPayable(payable('OPEN', 1)), false, '付过一分钱就不能作废');
  assert.equal(canCancelPayable(payable('PARTIAL_PAID', 100)), false);
  assert.equal(canWithdrawCancel(payable('OPEN', 0), true), false, '只有作废审批中才有撤回');
  assert.equal(isPendingCancel(null), false);
  assert.equal(canPayPayable(null), false);
});

test('② 没有撤回权限时说清原因（不静默隐藏）', () => {
  const blocked = withdrawCancelBlockedReason(payable('PENDING_CANCEL'), false);
  assert.ok(blocked.includes('business:erp:payable:withdraw-cancel'), '必须点名缺哪个权限码');
  assert.equal(withdrawCancelBlockedReason(payable('PENDING_CANCEL'), true), '');
  assert.equal(withdrawCancelBlockedReason(payable('OPEN'), false), '', '不是审批中就不用解释');
});

// ==================== ③ 详情抽屉「发起付款」的第四道闸 ====================

test('③ 后端 can_pay=true 但单据在作废审批中 → 抽屉不显示「发起付款」', () => {
  // 这是后端口径缺口（payment_capability 没有 PENDING_CANCEL 分支），前端必须自己兜
  assert.equal(canStartPay(payInfo(true), true, 'PENDING_CANCEL'), false);
  // 同一条信息在 OPEN 下仍然能付（别把日常路径闸掉）
  assert.equal(canStartPay(payInfo(true), true, 'OPEN'), true);
  // 省略 status = 老行为（老调用方 / 老用例不受影响）
  assert.equal(canStartPay(payInfo(true), true), true);
  assert.equal(canStartPay(payInfo(true), false, 'OPEN'), false, '没付款权限照旧不显示');
  assert.equal(canStartPay(null, true, 'OPEN'), false);
});

// ==================== ④ 送审提示文案（应付） ====================

test('④ 送审 → info 且只说「已提交审批」，绝不出现「已作废」', () => {
  const result = { outcome: OUTCOME_PENDING_APPROVAL, message: '已提交作废审批', approval_instance_id: 'FLW-1' };

  assert.equal(isPendingApproval(result), true);
  const feedback = submitFeedback('payable-cancel', result, 'AP-20261102-000001');
  assert.equal(feedback.level, 'info', '送审不是成功：账一分没动');
  assert.ok(feedback.text.includes('已提交审批，等待财务经理复核'), `文案缺关键句：${feedback.text}`);
  assert.ok(feedback.text.includes('AP-20261102-000001'), '必须带单号，列表里好定位');
  assert.ok(feedback.text.includes('未付'), '要说清单据仍是「未付」');
  assert.ok(!feedback.text.includes('已作废'), `送审不能提示「已作废」：${feedback.text}`);
  assert.equal(PENDING_APPROVAL_HINT, '已提交审批，等待财务经理复核');
});

test('④ 未配流程降级直做 → success「已作废：单号」（老文案保留）', () => {
  const feedback = submitFeedback(
    'payable-cancel',
    { outcome: OUTCOME_CANCELLED, message: 'x', approval_instance_id: null },
    'AP-20261102-000002',
  );
  assert.equal(feedback.level, 'success');
  assert.equal(feedback.text, '已作废：AP-20261102-000002');
  assert.equal(isPendingApproval({ outcome: OUTCOME_CANCELLED }), false);
});

test('④ 未知 outcome → 用后端 message 兜底，不吞掉也不瞎猜', () => {
  const feedback = submitFeedback('payable-cancel', { outcome: 'SOMETHING_NEW', message: '后端新文案' }, 'AP-1');
  assert.equal(feedback.level, 'info');
  assert.equal(feedback.text, '后端新文案');
  // 连 message 都没有时也要给出可读文案（不许出现 undefined）
  const bare = submitFeedback('payable-cancel', {}, 'AP-1');
  assert.ok(bare.text.includes('SOMETHING_NEW') === false);
  assert.ok(!bare.text.includes('undefined'));
});

// ==================== ⑤ 撤回提示（应付 / 凭证） ====================

test('⑤ 撤回文案说清「回到什么状态」，不让人以为单据被删了', () => {
  const cancel = withdrawFeedback('payable-cancel', 'AP-20261102-000003', '未付');
  assert.equal(cancel.level, 'success');
  assert.ok(cancel.text.includes('已撤回作废审批'));
  assert.ok(cancel.text.includes('回到「未付」'));

  const reverse = withdrawFeedback('voucher-reverse', 'PZ-20261102-000001', '已记账');
  assert.ok(reverse.text.includes('已撤回红冲审批'));
  assert.ok(reverse.text.includes('仍是「已记账」'), '凭证撤回后状态不变');
});

// ==================== ⑥ 凭证红冲送审文案 ====================

test('⑥ 凭证红冲送审 → info，绝不出现「已红冲」', () => {
  const feedback = submitFeedback(
    'voucher-reverse',
    { outcome: OUTCOME_PENDING_APPROVAL, message: 'x', approval_instance_id: 'FLW-2' },
    'PZ-20261102-000002',
    'PZ-20261102-000002',
  );
  assert.equal(feedback.level, 'info');
  assert.ok(feedback.text.includes('已提交审批，等待财务经理复核'));
  assert.ok(feedback.text.includes('已记账'), '要说清凭证仍「已记账」');
  assert.ok(!feedback.text.includes('已红冲'), `送审不能提示「已红冲」：${feedback.text}`);
  assert.ok(!feedback.text.includes('红字凭证 PZR'), '红字凭证还没生成，不能提它');
});

test('⑥ 未配流程降级直做 → success「已红冲：生成红字凭证 PZR-…」', () => {
  const feedback = submitFeedback(
    'voucher-reverse',
    { outcome: OUTCOME_REVERSED, message: 'x', approval_instance_id: null },
    'PZ-20261102-000003',
    'PZR-20261102-000001',
  );
  assert.equal(feedback.level, 'success');
  assert.equal(feedback.text, '已红冲：生成红字凭证 PZR-20261102-000001');
});

test('⑥ 徽标文案与卡面逐字一致', () => {
  assert.equal(pendingBadgeLabel('payable-cancel'), '作废审批中');
  assert.equal(pendingBadgeLabel('voucher-reverse'), '红冲审批中');
  assert.equal(VOUCHER_PENDING_REVERSE_LABEL, '红冲审批中');
});

// ==================== ⑦ 凭证：红冲审批中 → 「红冲」换成「撤回红冲」 ====================

test('⑦ 红冲审批中：不能红冲（必 409），但可以撤回；状态仍是 POSTED', () => {
  const pending = voucher({ pending_reverse_instance_id: 'f0000000-0000-4000-8000-000000000001' });

  assert.equal(isPendingReverse(pending), true);
  assert.equal(pending.status, 'POSTED', '红冲审批中状态不变（硬红线 3）');
  assert.equal(canReverseVoucher(pending), false, '审批中再点红冲后端 409');
  assert.equal(canWithdrawReverse(pending, true), true);
  assert.equal(canWithdrawReverse(pending, false), false, '没权限不给撤回入口');

  // 撤回后回到「可红冲」
  const after = voucher({ pending_reverse_instance_id: null });
  assert.equal(isPendingReverse(after), false);
  assert.equal(canReverseVoucher(after), true);
  assert.equal(canWithdrawReverse(after, true), false, '不在审批中就没有撤回可点');
});

test('⑦ 老的红冲判据原样不变（红字 / 已红冲 / 非 POSTED 都不可红冲）', () => {
  assert.equal(canReverseVoucher(voucher()), true);
  assert.equal(canReverseVoucher(voucher({ is_red: true })), false);
  assert.equal(canReverseVoucher(voucher({ reversed_by: 'uuid' })), false);
  assert.equal(canReverseVoucher(voucher({ reversed_by_no: 'PZR-1' })), false);
  assert.equal(canReverseVoucher(voucher({ status: 'DRAFT' })), false);
  assert.equal(canReverseVoucher(null), false);
  // 空串 / 空白不算「审批中」（后端 NULL vs '' 的差异不许被当成 pending）
  assert.equal(isPendingReverse({ pending_reverse_instance_id: '' }), false);
  assert.equal(isPendingReverse({ pending_reverse_instance_id: '  ' }), false);
  assert.equal(isPendingReverse({}), false);
});

test('⑦ 没有撤回权限时凭证侧也说清原因', () => {
  const pending = voucher({ pending_reverse_instance_id: 'f0000000-0000-4000-8000-000000000002' });
  const blocked = withdrawReverseBlockedReason(pending, false);
  assert.ok(blocked.includes('business:erp:voucher:withdraw-reverse'));
  assert.equal(withdrawReverseBlockedReason(pending, true), '');
  assert.equal(withdrawReverseBlockedReason(voucher(), false), '');
});

// ==================== ⑧ 契约收口：兜底已删，字段以 DTO 为准 ====================

/** 前端源码（相对 `frontend/src`，本文件在 `frontend/tests/unit`）。 */
function frontendSource(relative: string): string {
  return readFileSync(new URL(`../../src/${relative}`, import.meta.url), 'utf8');
}

test('⑧ 凭证页不再有「审批实例分页」兜底（删干净，grep 无引用）', () => {
  const page = frontendSource('views/business/erp/voucher/index.vue');
  assert.ok(!page.includes('loadPendingReverseFallback'), '兜底函数还在（或还有引用）');
  assert.ok(!page.includes('pageFlowInstances'), '还在调审批实例分页');
  assert.ok(!page.includes('VOUCHER_REVERSE'), '还在按 VOUCHER_REVERSE 查实例');
  assert.ok(!page.includes('pendingReverseMap'), '兜底映射表还在');
  // 徽标判据只剩后端下发的字段
  assert.ok(page.includes('pending_reverse_instance_id'), '列表 / 详情还要读这个字段');

  const logic = frontendSource('views/business/erp/voucher/voucher-logic.ts');
  assert.ok(!logic.includes('pendingReverseInstances'), '逻辑层的兜底映射还在');
  assert.ok(!logic.includes('VOUCHER_REVERSE_BIZ_TYPE'), 'biz_type 常量还在（已无消费者）');
});

test('⑧ 新字段是「恒在的 string | null」：前端必填 + 后端 DTO 真的下发', () => {
  // 前端：谁把字段改回可选，说明又在给「缺字段」留门
  const voucherApi = frontendSource('views/business/erp/voucher/voucher-write-api.ts');
  assert.ok(voucherApi.includes('pending_reverse_instance_id: string | null;'), 'VoucherFlowState 字段声明变了');
  assert.ok(!voucherApi.includes('pending_reverse_instance_id?:'), 'pending_reverse_instance_id 又变回可选');
  assert.ok(voucherApi.includes('reverse_reason: string | null;'), 'reverse_reason 字段声明变了');
  assert.ok(!voucherApi.includes('reverse_reason?:'), 'reverse_reason 又变回可选');

  const erpApi = frontendSource('api/business/erp/index.ts');
  assert.ok(erpApi.includes('cancel_instance_id: string | null;'), 'PayableRow 没声明 cancel_instance_id');
  assert.ok(erpApi.includes('cancel_reason: string | null;'), 'PayableRow 没声明 cancel_reason');

  // 后端：VoucherRes / PayableRes 必须真的带上（源码即契约，后端删字段这里先红）
  const voucherQuery = backendSource('voucher_query.rs');
  assert.ok(voucherQuery.includes('pub pending_reverse_instance_id: Option<String>'), 'VoucherRes 丢了 pending_reverse_instance_id');
  assert.ok(voucherQuery.includes('pub reverse_reason: Option<String>'), 'VoucherRes 丢了 reverse_reason');
  const payableQuery = backendSource('payable_query.rs');
  assert.ok(payableQuery.includes('pub cancel_instance_id: Option<String>'), 'PayableRes 丢了 cancel_instance_id');
  assert.ok(payableQuery.includes('pub cancel_reason: Option<String>'), 'PayableRes 丢了 cancel_reason');
});

// ==================== ⑨ 详情抽屉：作废进度 / 原因读 DTO 字段（重开抽屉仍在） ====================

test('⑨ 抽屉进度按单据状态选：作废审批中 → 作废实例，否则付款实例', () => {
  // 作废审批中 → 用作废实例 + 对应标题（标题写错会让「显示的是付款审批」这种误导回归）
  assert.deepEqual(approvalTargetOf({ status: 'PENDING_CANCEL', cancel_instance_id: 'FLW-cancel' }, 'FLW-pay'), {
    instanceId: 'FLW-cancel',
    title: '作废审批进度',
  });
  // 非作废审批中 → 付款审批（哪怕 cancel_instance_id 还有残留值）
  assert.deepEqual(approvalTargetOf({ status: 'OPEN', cancel_instance_id: 'FLW-old' }, 'FLW-pay'), {
    instanceId: 'FLW-pay',
    title: '付款审批进度',
  });
  assert.deepEqual(approvalTargetOf({ status: 'OPEN', cancel_instance_id: null }, null), {
    instanceId: null,
    title: '付款审批进度',
  });
  // 作废审批中却拿不到实例 id（不该发生）→ 不假装有进度
  assert.deepEqual(approvalTargetOf({ status: 'PENDING_CANCEL', cancel_instance_id: null }, null), {
    instanceId: null,
    title: '付款审批进度',
  });
  // 空串不算实例 id（NULL vs '' 的口径别混）
  assert.deepEqual(approvalTargetOf({ status: 'PENDING_CANCEL', cancel_instance_id: '  ' }, ''), {
    instanceId: null,
    title: '付款审批进度',
  });
  assert.deepEqual(approvalTargetOf(null, undefined), { instanceId: null, title: '付款审批进度' });
});

test('⑨ 作废原因：有值原样显示，null / 空白不渲染空行', () => {
  assert.equal(cancelReasonText({ cancel_reason: '供应商发票开错' }), '供应商发票开错');
  assert.equal(cancelReasonText({ cancel_reason: '  发票联次缺失  ' }), '发票联次缺失');
  assert.equal(cancelReasonText({ cancel_reason: null }), '');
  assert.equal(cancelReasonText({}), '');
  assert.equal(cancelReasonText(null), '');
});

test('⑨ outcome 常量与后端逐字一致（送审/直做两条路的判据来源）', () => {
  assert.equal(OUTCOME_PENDING_APPROVAL, 'PENDING_APPROVAL');
  assert.ok(backendSource('payable_payment.rs').includes('pub const OUTCOME_CANCEL_PENDING: &str = "PENDING_APPROVAL";'));
  assert.ok(backendSource('voucher_write.rs').includes('pub const OUTCOME_REVERSE_PENDING: &str = "PENDING_APPROVAL";'));
  assert.equal(OUTCOME_CANCELLED, 'CANCELLED');
  assert.equal(OUTCOME_REVERSED, 'REVERSED');
});
