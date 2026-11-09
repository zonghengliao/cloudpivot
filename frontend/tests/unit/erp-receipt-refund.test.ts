/**
 * `receipt-refund-logic.ts` 单测（NEAR-FIX-1 / 收款退款单页的纯逻辑）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里。下面这些一旦漂移
 * `vue-tsc` 抓不到（编译期两边都是 `string`），但线上会**算错钱或点不动按钮**：
 *
 * | 断言 | 后端出处 | 漂移后果 |
 * |------|----------|----------|
 * | 状态域 4 值 | `receipt_refund::STATUSES` | 传 OPEN → 400 |
 * | 「审批中」只能看 `approval_status` | 模块文档：「审批不是一个状态」 | 撤回按钮永不出现 |
 * | 过账要求核销满 | `change_status` 的 POSTED 分支 | 409，用户以为按钮坏了 |
 * | 送审要求核销满 | `submit_approval` 前置 | 审批通过后过账被拦，单子卡住 |
 * | 单张冲减 ≤ 已收 + 本单已冲减 | `validate_allocate_amount` | 改金额被误判超额 |
 * | 冲减合计 ≤ 退款金额 | `validate_allocate_total` | 400 且用户已填完一屏 |
 * | 原因按**字符**数 ≥ 4 | `REASON_MIN_CHARS` | 2 个中文被当 6 字节放过 |
 * | 空筛选不发空串 | `RefundPageQuery::validate` | `?status=` 查 0 行 |
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { ReceiptRefund } from '@/api/business/erp/receipt-refund';
import type { Receivable } from '@/api/business/erp';
import {
  REFUND_COLUMNS,
  REFUND_METHOD_OPTIONS,
  REFUND_PERMS,
  REFUND_REASON_MAX,
  REFUND_REASON_MIN,
  REFUND_STATUS_OPTIONS,
  approvalLabel,
  approvalTheme,
  buildAllocationPayload,
  buildAllocationRows,
  buildRefundQuery,
  canCancel,
  canConfirm,
  canPost,
  canSubmitApproval,
  canWithdraw,
  cancelBlockedReason,
  emptyRefundFilter,
  isAllocatable,
  isRefundFiltered,
  postBlockedReason,
  receiptRefundableCap,
  refundStatusTheme,
  refundableCents,
  submitBlockedReason,
  unallocatedCents,
  validateAllocations,
  validateRefundAmount,
  validateRefundReason,
  type AllocationRow,
} from '../../src/views/business/erp/receipt-refund/receipt-refund-logic';

/** 造一张退款单（只填被测字段，其余给安全的默认值） */
function refund(patch: Partial<ReceiptRefund> = {}): ReceiptRefund {
  return {
    id: 'r-1',
    refund_no: 'RR-20261110-000001',
    customer_id: 'c-1',
    customer_name: '客户甲',
    source_receipt_id: null,
    source_receipt_no: null,
    refund_date: '2026-11-10',
    amount: '100.0000',
    allocated_amount: '0.0000',
    unallocated_amount: '100.0000',
    method: 'BANK_TRANSFER',
    method_label: '银行转账',
    bank_account: null,
    status: 'DRAFT',
    status_label: '草稿',
    voucher_id: null,
    approval_instance_id: null,
    approval_instance_no: null,
    approval_status: null,
    approval_status_label: null,
    reason: '客户退货，货款原路退回',
    remark: null,
    created_at: null,
    updated_at: null,
    ...patch,
  };
}

/** 造一张应收（只填被测字段） */
function receivable(patch: Partial<Receivable> & { id: string; ar_no: string }): Receivable {
  return {
    customer_id: 'c-1',
    customer_name: '客户甲',
    source_type: 'SALES_OUT',
    source_id: 's-1',
    source_no: 'SO-20261110-000001',
    invoice_no: null,
    invoice_date: null,
    currency_code: 'CNY',
    amount: '1000.0000',
    received_amount: '0.0000',
    unsettled_amount: '1000.0000',
    credit_days: 0,
    due_date: '2026-11-30',
    period_code: '2026-11',
    status: 'OPEN',
    status_label: '未收',
    overdue: false,
    settled_at: null,
    voucher_id: null,
    remark: null,
    created_at: '2026-11-10T00:00:00Z',
    updated_at: '2026-11-10T00:00:00Z',
    ...patch,
  };
}

function allocRow(patch: Partial<AllocationRow> = {}): AllocationRow {
  return {
    receivableId: 'ar-1',
    arNo: 'AR-20261110-000001',
    arAmountCents: 100_000,
    receivedCents: 100_000,
    alreadyCents: 0,
    inputAmount: null,
    status: 'SETTLED',
    ...patch,
  };
}

// ==================== 常量口径 ====================

test('状态域是 4 值且与后端 STATUSES 逐字一致（不是应收那套）', () => {
  assert.deepEqual(
    REFUND_STATUS_OPTIONS.map((item) => item.value),
    ['DRAFT', 'CONFIRMED', 'POSTED', 'CANCELLED'],
  );
});

test('退款方式域是 4 值（METHODS）', () => {
  assert.deepEqual(
    REFUND_METHOD_OPTIONS.map((item) => item.value),
    ['BANK_TRANSFER', 'CASH', 'CHECK', 'OTHER'],
  );
});

test('权限码与 V0295 的 sys_menu.auth_code 逐字一致', () => {
  assert.equal(REFUND_PERMS.list, 'business:erp:receipt-refund:list');
  assert.equal(REFUND_PERMS.detail, 'business:erp:receipt-refund:detail');
  assert.equal(REFUND_PERMS.create, 'business:erp:receipt-refund:create');
  assert.equal(REFUND_PERMS.allocate, 'business:erp:receipt-refund:allocate');
  assert.equal(REFUND_PERMS.status, 'business:erp:receipt-refund:status');
});

test('列表必须有「来源收款单号」列（只给 UUID 等于让人查库）', () => {
  const keys = REFUND_COLUMNS.map((col) => col.colKey);
  for (const key of ['refund_no', 'source_receipt_no', 'customer_name', 'amount', 'status', 'refund_date']) {
    assert.ok(keys.includes(key), `列表缺列 ${key}`);
  }
});

// ==================== 状态配色 / 审批文案 ====================

test('状态 tag 配色：草稿灰、已确认蓝、已过账绿、已作废红', () => {
  assert.equal(refundStatusTheme('DRAFT'), 'default');
  assert.equal(refundStatusTheme('CONFIRMED'), 'primary');
  assert.equal(refundStatusTheme('POSTED'), 'success');
  assert.equal(refundStatusTheme('CANCELLED'), 'danger');
});

test('审批文案：null 必须显示「未送审」而不是空白', () => {
  assert.equal(approvalLabel(null), '未送审');
  assert.equal(approvalLabel(undefined), '未送审');
  assert.equal(approvalLabel('PENDING'), '审批中');
  assert.equal(approvalLabel('APPROVED'), '已通过');
  assert.equal(approvalLabel('REJECTED'), '已驳回');
  assert.equal(approvalLabel('WITHDRAWN'), '已撤回');
  assert.equal(approvalLabel('CANCELLED'), '已终止');
  assert.equal(approvalTheme('PENDING'), 'warning');
  assert.equal(approvalTheme(null), 'default');
});

// ==================== 按钮判据 ====================

test('确认：只有 DRAFT 能确认', () => {
  assert.equal(canConfirm(refund({ status: 'DRAFT' })), true);
  assert.equal(canConfirm(refund({ status: 'CONFIRMED' })), false);
  assert.equal(canConfirm(refund({ status: 'POSTED' })), false);
  assert.equal(canConfirm(refund({ status: 'CANCELLED' })), false);
});

test('过账：CONFIRMED 且核销满才亮', () => {
  const full = refund({ status: 'CONFIRMED', allocated_amount: '100.0000' });
  assert.equal(unallocatedCents(full), 0);
  assert.equal(canPost(full), true);

  const half = refund({ status: 'CONFIRMED', allocated_amount: '40.0000' });
  assert.equal(unallocatedCents(half), 6000);
  assert.equal(canPost(half), false);
  assert.match(postBlockedReason(half) ?? '', /未冲减余额 60\.00/);

  // 草稿不能直接过账
  assert.equal(canPost(refund({ status: 'DRAFT' })), false);
  assert.match(postBlockedReason(refund({ status: 'DRAFT' })) ?? '', /先「确认」/);
});

test('提交审批：核销满 + 审批不在跑 → 可提；approval_status=PENDING → 不可提', () => {
  const ready = refund({ status: 'CONFIRMED', allocated_amount: '100.0000' });
  assert.equal(canSubmitApproval(ready), true);

  const pending = refund({
    status: 'CONFIRMED',
    allocated_amount: '100.0000',
    approval_status: 'PENDING',
    approval_instance_id: 'i-1',
    approval_instance_no: 'FLW-20261110-00000001',
  });
  assert.equal(canSubmitApproval(pending), false);
  assert.match(submitBlockedReason(pending) ?? '', /已在审批中/);

  // 驳回后可以再提（approval_status = REJECTED 不是 PENDING）
  const rejected = refund({ status: 'CONFIRMED', allocated_amount: '100.0000', approval_status: 'REJECTED' });
  assert.equal(canSubmitApproval(rejected), true);

  // 没收满不能送审（送审后审批通过会直接过账，必然 409）
  const half = refund({ status: 'CONFIRMED', allocated_amount: '10.0000' });
  assert.equal(canSubmitApproval(half), false);
  assert.match(submitBlockedReason(half) ?? '', /未冲减余额/);
});

test('撤回：只看 approval_status === PENDING（审批不是单据状态）', () => {
  // status 仍是 CONFIRMED —— 用 status 判的话撤回按钮永远不出现
  const pending = refund({
    status: 'CONFIRMED',
    approval_status: 'PENDING',
    approval_instance_id: 'i-1',
  });
  assert.equal(canWithdraw(pending), true);

  // 没有实例 id 就撤不了（PENDING 但实例被清理是异常态）
  assert.equal(canWithdraw(refund({ approval_status: 'PENDING', approval_instance_id: null })), false);
  assert.equal(canWithdraw(refund({ approval_status: 'APPROVED', approval_instance_id: 'i-1' })), false);
  assert.equal(canWithdraw(refund({ status: 'CONFIRMED' })), false);
});

test('作废：DRAFT 且一分未冲减', () => {
  assert.equal(canCancel(refund({ status: 'DRAFT' })), true);
  assert.equal(canCancel(refund({ status: 'DRAFT', allocated_amount: '10.0000' })), false);
  assert.match(cancelBlockedReason(refund({ status: 'DRAFT', allocated_amount: '10.0000' })) ?? '', /已发生冲减/);
  assert.equal(canCancel(refund({ status: 'CONFIRMED' })), false);
  assert.equal(canCancel(refund({ status: 'POSTED' })), false);
  assert.match(cancelBlockedReason(refund({ status: 'POSTED' })) ?? '', /终止态|不能作废/);
});

test('可编辑核销：DRAFT/CONFIRMED 可改，审批中 / 过账后锁死', () => {
  assert.equal(isAllocatable(refund({ status: 'DRAFT' })), true);
  assert.equal(isAllocatable(refund({ status: 'CONFIRMED' })), true);
  assert.equal(isAllocatable(refund({ status: 'CONFIRMED', approval_status: 'PENDING' })), false);
  assert.equal(isAllocatable(refund({ status: 'POSTED' })), false);
  assert.equal(isAllocatable(refund({ status: 'CANCELLED' })), false);
});

// ==================== 金额 / 冲减 ====================

test('可退金额 = 应收已收 + 本单此前已冲减（不是直接减）', () => {
  assert.equal(refundableCents(10_000), 10_000);
  assert.equal(refundableCents(0, 5_000), 5_000);
  assert.equal(refundableCents(4_000, 6_000), 10_000);
  assert.equal(refundableCents(0, 0), 0);
  // 负数不收（库层 amount > 0，冲减只能冲正数）
  assert.equal(refundableCents(-100), 0);
});

test('候选行只保留「收过钱」的应收，并按单号升序（先冲最早的账）', () => {
  const rows = buildAllocationRows(
    [
      receivable({ id: 'a2', ar_no: 'AR-20261110-000002', received_amount: '50.0000', status: 'PARTIAL' }),
      receivable({ id: 'a1', ar_no: 'AR-20261110-000001', received_amount: '80.0000', status: 'PARTIAL' }),
      // 一分没收的应收不列出来（退款只能退已经收到的钱）
      receivable({ id: 'a3', ar_no: 'AR-20261110-000003', received_amount: '0.0000', status: 'OPEN' }),
    ],
  );
  assert.deepEqual(
    rows.map((row) => row.arNo),
    ['AR-20261110-000001', 'AR-20261110-000002'],
  );
  assert.equal(rows[0].receivedCents, 8000);
  assert.equal(rows[0].inputAmount, null);
});

test('候选行预填本单已冲减金额（编辑既有单时不能当成 0 再冲一遍）', () => {
  const rows = buildAllocationRows(
    [receivable({ id: 'a1', ar_no: 'AR-20261110-000001', received_amount: '30.0000', status: 'SETTLED' })],
    [{ receivable_id: 'a1', amount: '70.0000' }],
  );
  assert.equal(rows.length, 1);
  assert.equal(rows[0].alreadyCents, 7000);
  // 可退 = 30 + 70 = 100（已冲减的部分要先加回来）
  assert.equal(refundableCents(rows[0].receivedCents, rows[0].alreadyCents), 10_000);
  assert.equal(rows[0].inputAmount, 70);
});

test('冲减校验：单张超额 → 报错并点名哪张应收', () => {
  const rows = [allocRow({ arNo: 'AR-20261110-000001', receivedCents: 5000, inputAmount: 60 })];
  const check = validateAllocations(rows, 100_000);
  assert.ok(check.error);
  assert.match(check.error ?? '', /AR-20261110-000001/);
  assert.match(check.error ?? '', /超过可退 50\.00/);
});

test('冲减校验：合计超退款金额 → 报错', () => {
  const rows = [
    allocRow({ receivableId: 'a1', arNo: 'AR-1', receivedCents: 80_000, inputAmount: 80 }),
    allocRow({ receivableId: 'a2', arNo: 'AR-2', receivedCents: 80_000, inputAmount: 50 }),
  ];
  const check = validateAllocations(rows, 10_000); // 退款 100
  assert.ok(check.error);
  assert.match(check.error ?? '', /冲减合计 130\.00 超过退款金额 100\.00/);
  assert.equal(check.totalCents, 13_000);
});

test('冲减校验：一行都没填 → 报错（allocate 的 items 不能为空）', () => {
  const check = validateAllocations([allocRow({ inputAmount: null })], 10_000);
  assert.ok(check.error);
  assert.match(check.error ?? '', /至少填一张应收/);
});

test('冲减校验：正好填满 → 通过，合计正确', () => {
  const rows = [
    allocRow({ receivableId: 'a1', arNo: 'AR-1', receivedCents: 70_000, inputAmount: 60 }),
    allocRow({ receivableId: 'a2', arNo: 'AR-2', receivedCents: 40_000, inputAmount: 40 }),
  ];
  const check = validateAllocations(rows, 10_000);
  assert.equal(check.error, null);
  assert.equal(check.filledCount, 2);
  assert.equal(check.totalCents, 10_000);
});

test('冲减入参只发有金额的行（勾掉 = 不发，不是发 0）', () => {
  const rows = [
    allocRow({ receivableId: 'a1', arNo: 'AR-1', inputAmount: 12.345 }),
    allocRow({ receivableId: 'a2', arNo: 'AR-2', inputAmount: null }),
    allocRow({ receivableId: 'a3', arNo: 'AR-3', inputAmount: 0 }),
  ];
  const payload = buildAllocationPayload(rows);
  assert.equal(payload.length, 1);
  assert.equal(payload[0].receivable_id, 'a1');
  // 只保留 2 位（后端 NUMERIC(18,4) 收得进，但多给的位数没有业务含义）
  assert.equal(payload[0].amount, 12.35);
});

// ==================== 原因 / 金额入参校验 ====================

test('退款原因按字符数校验（不是字节）：2 个中文不放过、4 个通过', () => {
  assert.equal(REFUND_REASON_MIN, 4);
  assert.match(validateRefundReason('退货') ?? '', /至少 4 个字/);
  assert.match(validateRefundReason('   ') ?? '', /必填/);
  assert.equal(validateRefundReason('客户退货'), null);
  // 长度上限：501 个字拒绝，500 个通过
  assert.match(validateRefundReason('退'.repeat(REFUND_REASON_MAX + 1)) ?? '', /最多/);
  assert.equal(validateRefundReason('退'.repeat(REFUND_REASON_MAX)), null);
});

test('退款金额：必须 > 0，且不超过来源收款单可退上限', () => {
  assert.match(validateRefundAmount(0, null) ?? '', /大于 0/);
  assert.match(validateRefundAmount(-100, null) ?? '', /大于 0/);
  assert.equal(validateRefundAmount(10_000, null), null);
  assert.equal(validateRefundAmount(10_000, 10_000), null);
  assert.match(validateRefundAmount(10_001, 10_000) ?? '', /超过该收款单可退金额/);
});

test('来源收款单可退上限 = 去重后的应收 received_amount 之和', () => {
  const map = new Map([
    ['a1', 5000],
    ['a2', 3000],
  ]);
  // 同一张应收被两条核销明细指向（不可能但去重逻辑要成立）
  const cap = receiptRefundableCap(
    [{ receivable_id: 'a1' }, { receivable_id: 'a2' }, { receivable_id: 'a1' }],
    map,
  );
  assert.equal(cap, 8000);
});

// ==================== 筛选入参 ====================

test('空筛选 → 只发 page/page_size（空串不发，后端会把 ?status= 当等值过滤查 0 行）', () => {
  const query = buildRefundQuery(emptyRefundFilter(), 1);
  assert.deepEqual(query, { page: 1, page_size: 20 });
  assert.equal(isRefundFiltered(emptyRefundFilter()), false);
});

test('筛选 → 参数名逐字对齐 RefundPageQuery', () => {
  const query = buildRefundQuery(
    {
      status: 'CONFIRMED',
      customerId: 'c-1',
      method: 'CASH',
      refundNo: 'RR-2026',
      dateFrom: '2026-11-01',
      dateTo: '2026-11-30',
    },
    2,
  );
  assert.deepEqual(query, {
    page: 2,
    page_size: 20,
    status: 'CONFIRMED',
    customer_id: 'c-1',
    method: 'CASH',
    refund_no: 'RR-2026',
    date_from: '2026-11-01',
    date_to: '2026-11-30',
  });
  assert.equal(isRefundFiltered({ ...emptyRefundFilter(), refundNo: 'x' }), true);
});
