/**
 * `receipt-logic.ts` 单测（SO-18 收款单页的纯逻辑）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里。下面这些一旦漂移
 * `vue-tsc` 抓不到（编译期两边都是 `string`），但线上会**算错钱或点不动按钮**：
 *
 * | 断言 | 后端出处 | 漂移后果 |
 * |------|----------|----------|
 * | 状态域 4 值 ≠ 应收那套 | `receipt::STATUSES` / `ck_biz_receipt_status` | 传 `OPEN` → 400 |
 * | 过账要求核销满 | `change_status`：`allocated == amount` | 409，用户以为按钮坏了 |
 * | 核销上限 = 未收 + 本收款已核 | `allocate` 的 `cap` | 改大金额被误判超额 |
 * | 未改金额的行不发 | `allocate` 对每条 items 校验 `has_balance(应收状态)` | 重提已核满的应收 → 409（本卡实测踩到） |
 * | 作废原因按**字符**数 | `receipt::REASON_MIN_CHARS = 4` | 中文 2 字被当 6 字节放过 |
 * | 空筛选不发空串 | `ReceiptPageQuery::validate` | `?status=` 查 0 行、`?customer_id=` 400 |
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Receipt, ReceiptAllocation } from '@/api/business/erp';
import {
  ALLOCATION_FETCH_LIMIT,
  RECEIPT_COLUMNS,
  RECEIPT_EDGES,
  RECEIPT_LOCKED,
  RECEIPT_METHODS,
  RECEIPT_METHOD_LABELS,
  RECEIPT_METHOD_OPTIONS,
  RECEIPT_PAGE_SIZE,
  RECEIPT_PERMS,
  RECEIPT_REASON_EDGES,
  RECEIPT_REASON_MAX,
  RECEIPT_REASON_MIN,
  RECEIPT_STATUSES,
  RECEIPT_STATUS_LABELS,
  RECEIPT_STATUS_OPTIONS,
  allocationArStatusLabel,
  allocationSummary,
  allocatableCents,
  allocatedPercent,
  buildAllocateItems,
  buildAllocationRows,
  buildReceiptQuery,
  canConfirm,
  canCancel,
  canPost,
  cancelBlockedReason,
  changedAllocationRows,
  createFormError,
  emptyReceiptFilter,
  fillSequentially,
  hasAllocationChanges,
  isAllocatable,
  isReceiptFiltered,
  mergeAllocatableReceivables,
  needsReason,
  postBlockedReason,
  receivableAllocatable,
  receiptMethodLabel,
  receiptReasonError,
  receiptStatusLabel,
  receiptStatusTheme,
  remainingAfterAllocation,
  rowRemainingCents,
  unallocatedCents,
  validateAllocation,
} from '@/views/business/erp/receipt/receipt-logic';

/** 一行真数据（形态照抄 `GET /api/business/erp/receipt/page` 的 list[0]） */
function row(overrides: Partial<Receipt> = {}): Receipt {
  return {
    id: '11111111-1111-1111-1111-111111111111',
    receipt_no: 'RC-20261106-000001',
    customer_id: '22222222-2222-2222-2222-222222222222',
    customer_name: '北京云枢科技有限公司',
    receipt_date: '2026-11-06',
    amount: '1000.0000',
    allocated_amount: '0.0000',
    unallocated_amount: '1000.0000',
    method: 'BANK_TRANSFER',
    method_label: '银行转账',
    bank_account: '6222-SO18',
    status: 'DRAFT',
    status_label: '草稿',
    voucher_id: null,
    remark: null,
    created_at: null,
    updated_at: null,
    ...overrides,
  };
}

/** 一张可核销应收（`GET /receivable/page` 的子集） */
function ar(overrides: Record<string, unknown> = {}) {
  return {
    id: 'aaaaaaaa-0000-4000-8000-000000000001',
    ar_no: 'AR-SO18-001',
    customer_name: '北京云枢科技有限公司',
    due_date: '2026-11-05',
    amount: '600.0000',
    received_amount: '0.0000',
    unsettled_amount: '600.0000',
    status: 'OPEN',
    status_label: '未收',
    ...overrides,
  } as Parameters<typeof buildAllocationRows>[0][number];
}

// ==================== 域值 / 文案 ====================

test('状态域是收款那 4 个值，绝不能混进应收的 OPEN/PARTIAL', () => {
  assert.deepEqual(RECEIPT_STATUSES, ['DRAFT', 'CONFIRMED', 'POSTED', 'CANCELLED']);
  for (const alien of ['OPEN', 'PARTIAL', 'SETTLED', 'WRITTEN_OFF']) {
    assert.ok(!RECEIPT_STATUSES.includes(alien as never), `${alien} 不该出现在收款状态域`);
  }
});

test('收款方式域与中文标签逐字对齐后端', () => {
  assert.deepEqual(RECEIPT_METHODS, ['BANK_TRANSFER', 'CASH', 'CHECK', 'OTHER']);
  assert.deepEqual(RECEIPT_METHOD_OPTIONS.map((o) => o.label), ['银行转账', '现金', '支票', '其它']);
  assert.equal(RECEIPT_METHOD_OPTIONS.length, RECEIPT_METHODS.length);
});

test('状态下拉 value 用编码、label 用中文（回填时 v-model 传的是编码）', () => {
  assert.deepEqual(
    RECEIPT_STATUS_OPTIONS.map((o) => o.value),
    RECEIPT_STATUSES,
  );
  assert.equal(RECEIPT_STATUS_OPTIONS[0]?.label, RECEIPT_STATUS_LABELS.DRAFT);
});

test('状态机只有 3 条边：没有 CONFIRMED→CANCELLED、没有 POSTED→*', () => {
  assert.equal(RECEIPT_EDGES.length, 3);
  assert.ok(!RECEIPT_EDGES.some(([from]) => from === 'POSTED'));
  assert.ok(!RECEIPT_EDGES.some(([from, to]) => from === 'CONFIRMED' && to === 'CANCELLED'));
  assert.deepEqual(RECEIPT_LOCKED, ['POSTED', 'CANCELLED']);
  assert.equal(needsReason('DRAFT', 'CANCELLED'), true);
  assert.equal(needsReason('DRAFT', 'CONFIRMED'), false);
  assert.deepEqual(RECEIPT_REASON_EDGES, [['DRAFT', 'CANCELLED']]);
});

test('权限码 5 个，与 V0293 种的 auth_code 逐字一致', () => {
  assert.deepEqual(Object.values(RECEIPT_PERMS), [
    'business:erp:receipt:list',
    'business:erp:receipt:detail',
    'business:erp:receipt:create',
    'business:erp:receipt:allocate',
    'business:erp:receipt:status',
  ]);
});

test('状态中文：后端 status_label 优先，缺失回落本地表，未知状态回落编码', () => {
  assert.equal(receiptStatusLabel(row({ status: 'POSTED', status_label: '已入账' })), '已入账');
  assert.equal(receiptStatusLabel(row({ status: 'POSTED', status_label: '' })), '已过账');
  assert.equal(receiptStatusLabel({ status: 'WEIRD', status_label: '' } as Receipt), 'WEIRD');
  assert.equal(receiptStatusTheme('POSTED'), 'success');
  assert.equal(receiptStatusTheme('WEIRD'), 'default');
});

test('收款方式中文：后端 method_label 优先，空方法给「未指定」', () => {
  assert.equal(receiptMethodLabel(row()), '银行转账');
  assert.equal(receiptMethodLabel({ method: 'CASH', method_label: '' } as Receipt), '现金');
  assert.equal(receiptMethodLabel({ method: null, method_label: '' } as Receipt), '未指定');
  assert.equal(RECEIPT_METHOD_LABELS.CHECK, '支票');
});

// ==================== 列表列 / 筛选 ====================

test('列表列覆盖卡面要求的 6 列（单号/客户/日期/金额/已核销/状态）', () => {
  const keys = RECEIPT_COLUMNS.map((col) => col.colKey);
  for (const required of [
    'receipt_no',
    'customer_name',
    'receipt_date',
    'amount',
    'allocated_amount',
    'status',
  ]) {
    assert.ok(keys.includes(required), `缺列 ${required}`);
  }
  // 单号左固定、操作右固定；金额列右对齐
  assert.equal(RECEIPT_COLUMNS.find((c) => c.colKey === 'receipt_no')?.fixed, 'left');
  assert.equal(RECEIPT_COLUMNS.find((c) => c.colKey === 'op')?.fixed, 'right');
  assert.equal(RECEIPT_COLUMNS.find((c) => c.colKey === 'amount')?.align, 'right');
  assert.equal(RECEIPT_PAGE_SIZE, 20);
  assert.equal(ALLOCATION_FETCH_LIMIT, 200);
});

test('空筛选一律不发空串（后端把 ?status= 当等于空串、?customer_id= 当非法 UUID）', () => {
  const query = buildReceiptQuery(emptyReceiptFilter(), 1);
  assert.deepEqual(query, { page: 1, page_size: RECEIPT_PAGE_SIZE });
  assert.equal(isReceiptFiltered(emptyReceiptFilter()), false);
});

test('筛选参数：空格被 trim、单号模糊、日期下发 yyyy-MM-dd', () => {
  const filter = {
    ...emptyReceiptFilter(),
    status: 'CONFIRMED' as const,
    customerId: 'c-1',
    receiptNo: '  RC-2026  ',
    method: 'CASH' as const,
    dateFrom: '2026-11-01',
    dateTo: '2026-11-30',
  };
  assert.deepEqual(buildReceiptQuery(filter, 3), {
    page: 3,
    page_size: RECEIPT_PAGE_SIZE,
    status: 'CONFIRMED',
    customer_id: 'c-1',
    receipt_no: 'RC-2026',
    method: 'CASH',
    date_from: '2026-11-01',
    date_to: '2026-11-30',
  });
  assert.equal(isReceiptFiltered(filter), true);
  // 只填空格不算筛选
  assert.equal(isReceiptFiltered({ ...emptyReceiptFilter(), receiptNo: '   ' }), false);
  // 非法页码收敛到 1
  assert.equal(buildReceiptQuery(emptyReceiptFilter(), 0).page, 1);
});

// ==================== 状态动作 ====================

test('确认按钮：只有 DRAFT 能确认', () => {
  assert.equal(canConfirm(row({ status: 'DRAFT' })), true);
  for (const status of ['CONFIRMED', 'POSTED', 'CANCELLED']) {
    assert.equal(canConfirm(row({ status })), false, status);
  }
});

test('过账：必须 CONFIRMED 且核销满（allocated == amount），字符串金额也要比得准', () => {
  assert.equal(canPost(row({ status: 'CONFIRMED', allocated_amount: '1000.0000' })), true);
  assert.equal(canPost(row({ status: 'CONFIRMED', allocated_amount: '999.9900' })), false);
  assert.equal(canPost(row({ status: 'DRAFT', allocated_amount: '1000.0000' })), false);
  // 未核销满要给得出「还差多少」的人话文案
  const reason = postBlockedReason(row({ status: 'CONFIRMED', allocated_amount: '900.0000' }));
  assert.match(reason, /100\.00/);
  assert.match(reason, /未核销/);
  // 已过账 / 已作废 / 草稿各有各的提示
  assert.match(postBlockedReason(row({ status: 'POSTED' })), /已过账/);
  assert.match(postBlockedReason(row({ status: 'CANCELLED' })), /作废/);
  assert.match(postBlockedReason(row({ status: 'DRAFT' })), /先确认/);
  assert.equal(postBlockedReason(row({ status: 'CONFIRMED', allocated_amount: '1000.0000' })), '');
});

test('作废：只有「草稿 + 一分钱没核销」能作废；其余都要给出原因', () => {
  assert.equal(canCancel(row({ status: 'DRAFT', allocated_amount: '0.0000' })), true);
  assert.equal(cancelBlockedReason(row({ status: 'DRAFT', allocated_amount: '0.0000' })), '');
  assert.equal(canCancel(row({ status: 'DRAFT', allocated_amount: '100.0000' })), false);
  assert.match(
    cancelBlockedReason(row({ status: 'DRAFT', allocated_amount: '100.0000' })),
    /先反核销/,
  );
  assert.equal(canCancel(row({ status: 'CONFIRMED', allocated_amount: '0.0000' })), false);
  assert.match(cancelBlockedReason(row({ status: 'CONFIRMED' })), /红冲/);
  assert.equal(canCancel(row({ status: 'POSTED' })), false);
  assert.equal(canCancel(row({ status: 'CANCELLED' })), false);
});

test('可核销状态 = DRAFT | CONFIRMED（POSTED/CANCELLED 后端 409）', () => {
  assert.equal(isAllocatable('DRAFT'), true);
  assert.equal(isAllocatable('CONFIRMED'), true);
  assert.equal(isAllocatable('POSTED'), false);
  assert.equal(isAllocatable('CANCELLED'), false);
});

test('金额小工具：字符串小数不拼串、比例不除零、负数留符号', () => {
  assert.equal(unallocatedCents(row({ amount: '1000.0000', allocated_amount: '900.0000' })), 10000);
  assert.equal(allocatedPercent(row({ amount: '1000.0000', allocated_amount: '250.0000' })), 25);
  assert.equal(allocatedPercent(row({ amount: '0.0000', allocated_amount: '0.0000' })), 0);
  assert.equal(allocatedPercent(row({ amount: '100.0000', allocated_amount: '200.0000' })), 100);
  assert.equal(unallocatedCents(row({ amount: '0.1', allocated_amount: '0.2' })), -10);
});

// ==================== 作废原因 / 登记表单 ====================

test('作废原因按字符数：4 个汉字合法、3 个不合法、501 个超限', () => {
  assert.equal(RECEIPT_REASON_MIN, 4);
  assert.equal(RECEIPT_REASON_MAX, 500);
  assert.equal(receiptReasonError('客户退票'), '');
  assert.match(receiptReasonError('退票'), /至少 4 个字/);
  assert.match(receiptReasonError('  退票  '), /至少 4 个字/); // trim 后再算
  assert.match(receiptReasonError('票'.repeat(RECEIPT_REASON_MAX + 1)), /最多 500 个字/);
});

test('登记表单：客户必选、金额必须 > 0、日期形状 yyyy-MM-dd', () => {
  assert.match(createFormError({ customer_id: '', amount: '100' }), /请选择客户/);
  assert.match(createFormError({ customer_id: 'c-1', amount: '0' }), /大于 0/);
  assert.match(createFormError({ customer_id: 'c-1', amount: '-5' }), /大于 0/);
  assert.match(
    createFormError({ customer_id: 'c-1', amount: '100', receipt_date: '2026/11/06' }),
    /yyyy-MM-dd/,
  );
  assert.equal(
    createFormError({ customer_id: 'c-1', amount: '100.50', receipt_date: '2026-11-06' }),
    '',
  );
});

// ==================== 核销面板 ====================

test('面板行：已有核销回填勾选 + 金额，可核销余额 = 未收 + 本收款此前核过的', () => {
  const existing: ReceiptAllocation[] = [
    {
      id: 'al-1',
      receivable_id: 'aaaaaaaa-0000-4000-8000-000000000001',
      ar_no: 'AR-SO18-001',
      ar_amount: '600.0000',
      ar_received_amount: '600.0000',
      ar_unsettled_amount: '0.0000',
      ar_status: 'SETTLED',
      ar_status_label: '已收清',
      amount: '600.0000',
      remark: null,
      created_at: null,
    },
  ];
  // 该应收此刻已结清（未收 0），但本收款此前核过 600 ⇒ 可核销余额仍是 600
  const rows = buildAllocationRows(
    [ar({ unsettled_amount: '0.0000', received_amount: '600.0000', status: 'SETTLED' })],
    existing,
  );
  assert.equal(rows.length, 1);
  assert.equal(rows[0]?.priorCents, 60000);
  assert.equal(rows[0]?.allocated, 60000);
  assert.equal(rows[0]?.checked, true);
  assert.equal(allocatableCents(rows[0]!), 60000);
});

test('面板行：详情里有、未收列表没带回的应收也要留在面板上（否则保存时被静默丢掉）', () => {
  const existing: ReceiptAllocation[] = [
    {
      id: 'al-2',
      receivable_id: 'aaaaaaaa-0000-4000-8000-000000000009',
      ar_no: 'AR-SO18-009',
      ar_amount: '300.0000',
      ar_received_amount: '300.0000',
      ar_unsettled_amount: '0.0000',
      ar_status: 'SETTLED',
      ar_status_label: '已收清',
      amount: '300.0000',
      remark: null,
      created_at: null,
    },
  ];
  const rows = buildAllocationRows([], existing);
  assert.equal(rows.length, 1);
  assert.equal(rows[0]?.ar_no, 'AR-SO18-009');
  assert.equal(rows[0]?.priorCents, 30000);
});

test('面板行：同一张应收重复出现只保留一行', () => {
  const rows = buildAllocationRows([ar(), ar()]);
  assert.equal(rows.length, 1);
});

test('未收应收合并：去重、丢掉终态与未收 0 的行、按到期日升序', () => {
  const merged = mergeAllocatableReceivables([
    ar({ id: 'b', ar_no: 'AR-B', due_date: '2026-11-20' }),
    ar({ id: 'a', ar_no: 'AR-A', due_date: '2026-11-05' }),
    ar({ id: 'a', ar_no: 'AR-A', due_date: '2026-11-05' }), // 两页重叠
    ar({ id: 'c', ar_no: 'AR-C', status: 'SETTLED', unsettled_amount: '0.0000' }),
    ar({ id: 'd', ar_no: 'AR-D', status: 'OPEN', unsettled_amount: '0.0000' }),
  ]);
  assert.deepEqual(
    merged.map((item) => item.ar_no),
    ['AR-A', 'AR-B'],
  );
  assert.equal(receivableAllocatable('OPEN'), true);
  assert.equal(receivableAllocatable('PARTIAL'), true);
  assert.equal(receivableAllocatable('WRITTEN_OFF'), false);
});

test('校验：单张超可核销余额 / 合计超收款金额 / 填了没勾 / 一张没勾', () => {
  const rows = buildAllocationRows([
    ar({ id: 'a', ar_no: 'AR-A', unsettled_amount: '600.0000' }),
    ar({ id: 'b', ar_no: 'AR-B', unsettled_amount: '400.0000' }),
  ]);
  // 600 + 400 = 1000 ≤ 收款 1000 → 通过
  rows[0]!.checked = true;
  rows[0]!.allocated = 60000;
  rows[1]!.checked = true;
  rows[1]!.allocated = 40000;
  assert.equal(validateAllocation('1000.0000', rows), null);

  // 单张超额
  const over = rows.map((item) => ({ ...item }));
  over[0]!.allocated = 60001;
  assert.match(validateAllocation('1000.0000', over) ?? '', /超过应收 AR-A 的可核销余额 600\.00/);

  // 合计超额
  const total = buildAllocationRows([
    ar({ id: 'a', ar_no: 'AR-A', unsettled_amount: '600.0000' }),
    ar({ id: 'b', ar_no: 'AR-B', unsettled_amount: '400.0000' }),
  ]);
  total[0]!.checked = true;
  total[0]!.allocated = 60000;
  total[1]!.checked = true;
  total[1]!.allocated = 40000;
  assert.match(validateAllocation('900.0000', total) ?? '', /超过收款金额 900\.00/);

  // 填了金额但没勾选 → 报错而不是静默丢掉
  const dirty = rows.map((item) => ({ ...item }));
  dirty[1]!.checked = false;
  assert.match(validateAllocation('1000.0000', dirty) ?? '', /填了金额但没勾选/);

  // 一张都没勾
  const none = rows.map((item) => ({ ...item, checked: false, allocated: 0 }));
  assert.match(validateAllocation('1000.0000', none) ?? '', /至少勾选一张应收/);

  // 勾了但金额 0
  const zero = rows.map((item) => ({ ...item, allocated: 0 }));
  assert.match(validateAllocation('1000.0000', zero) ?? '', /必须大于 0/);
});

test('已核销的行不能取消勾选、不能调小（无反核销接口，后端 UPSERT 不删行）', () => {
  const existing: ReceiptAllocation[] = [
    {
      id: 'al-3',
      receivable_id: 'aaaaaaaa-0000-4000-8000-000000000001',
      ar_no: 'AR-SO18-001',
      ar_amount: '600.0000',
      ar_received_amount: '600.0000',
      ar_unsettled_amount: '0.0000',
      ar_status: 'SETTLED',
      ar_status_label: '已收清',
      amount: '600.0000',
      remark: null,
      created_at: null,
    },
  ];
  const rows = buildAllocationRows([ar({ unsettled_amount: '0.0000' })], existing);
  // 取消勾选
  const off = rows.map((item) => ({ ...item, checked: false, allocated: 0 }));
  assert.match(validateAllocation('1000.0000', off) ?? '', /不能取消勾选/);
  // 调小
  const less = rows.map((item) => ({ ...item, allocated: 50000 }));
  assert.match(validateAllocation('1000.0000', less) ?? '', /不能减少到 500\.00/);
  // 原样保留 → 通过
  assert.equal(validateAllocation('1000.0000', rows), null);
});

test('buildAllocateItems 跳过「金额没变」的行（重提已核满应收会被后端 409）', () => {
  const existing: ReceiptAllocation[] = [
    {
      id: 'al-4',
      receivable_id: 'aaaaaaaa-0000-4000-8000-000000000001',
      ar_no: 'AR-SO18-001',
      ar_amount: '600.0000',
      ar_received_amount: '600.0000',
      ar_unsettled_amount: '0.0000',
      ar_status: 'SETTLED',
      ar_status_label: '已收清',
      amount: '600.0000',
      remark: null,
      created_at: null,
    },
  ];
  const rows = buildAllocationRows(
    [ar({ id: 'aaaaaaaa-0000-4000-8000-000000000001', ar_no: 'AR-SO18-001', unsettled_amount: '0.0000' })],
    existing,
  );
  // 只有未变行 → 不发请求
  assert.equal(hasAllocationChanges(rows), false);
  assert.deepEqual(buildAllocateItems(rows), []);

  // 再加一张新的应收并核 100 → 只发这一张，不动已核满那张
  const withNew = buildAllocationRows(
    [
      ar({ id: 'aaaaaaaa-0000-4000-8000-000000000001', ar_no: 'AR-SO18-001', unsettled_amount: '0.0000' }),
      ar({ id: 'bbbbbbbb-0000-4000-8000-000000000002', ar_no: 'AR-SO18-002', unsettled_amount: '100.0000' }),
    ],
    existing,
  );
  const target = withNew.find((item) => item.ar_no === 'AR-SO18-002')!;
  target.checked = true;
  target.allocated = 10000;
  assert.equal(hasAllocationChanges(withNew), true);
  assert.deepEqual(buildAllocateItems(withNew), [
    { receivable_id: 'bbbbbbbb-0000-4000-8000-000000000002', amount: '100.0000' },
  ]);
  assert.equal(changedAllocationRows(withNew).length, 1);
});

test('核销金额出参是「元.4 位小数」定点串（后端 Decimal 收字符串）', () => {
  const rows = buildAllocationRows([ar({ unsettled_amount: '0.3000' })]);
  rows[0]!.checked = true;
  rows[0]!.allocated = 10; // 0.10 元
  assert.deepEqual(buildAllocateItems(rows), [
    { receivable_id: 'aaaaaaaa-0000-4000-8000-000000000001', amount: '0.1000' },
  ]);
});

test('实时剩余：合计 / 剩余 / 勾选张数 / 单行剩余', () => {
  const rows = buildAllocationRows([
    ar({ id: 'a', ar_no: 'AR-A', unsettled_amount: '600.0000' }),
    ar({ id: 'b', ar_no: 'AR-B', unsettled_amount: '400.0000' }),
  ]);
  rows[0]!.checked = true;
  rows[0]!.allocated = 60000;
  rows[1]!.checked = true;
  rows[1]!.allocated = 30000;
  const summary = allocationSummary('1000.0000', rows);
  assert.deepEqual(summary, {
    receiptCents: 100000,
    allocatedCents: 90000,
    remainingCents: 10000,
    checkedCount: 2,
  });
  assert.equal(remainingAfterAllocation('1000.0000', rows), 10000);
  assert.equal(rowRemainingCents(rows[1]!), 10000); // 400 − 300
  // 超额时剩余可以是负数（UI 用它报红）
  rows[1]!.allocated = 50000;
  assert.equal(remainingAfterAllocation('1000.0000', rows), -10000);
});

test('一键填满：按到期日顺序从上往下填，最后一张只填到剩余额度', () => {
  const rows = buildAllocationRows([
    ar({ id: 'a', ar_no: 'AR-A', due_date: '2026-11-05', unsettled_amount: '600.0000' }),
    ar({ id: 'b', ar_no: 'AR-B', due_date: '2026-11-20', unsettled_amount: '400.0000' }),
  ]);
  const filled = fillSequentially('700.0000', rows);
  assert.deepEqual(
    filled.map((item) => [item.ar_no, item.allocated, item.checked]),
    [
      ['AR-A', 60000, true],
      ['AR-B', 10000, true],
    ],
  );
  // 钱不够填满时后面的行不勾
  const partial = fillSequentially('100.0000', rows);
  assert.deepEqual(
    partial.map((item) => [item.ar_no, item.allocated, item.checked]),
    [
      ['AR-A', 10000, true],
      ['AR-B', 0, false],
    ],
  );
});

test('应收状态中文：后端 status_label 优先，缺失回落本地表', () => {
  assert.equal(allocationArStatusLabel({ status: 'PARTIAL', status_label: '部分已收' }), '部分已收');
  assert.equal(allocationArStatusLabel({ status: 'SETTLED', status_label: '' }), '已收清');
  assert.equal(allocationArStatusLabel({ status: 'XX', status_label: '' }), 'XX');
});
