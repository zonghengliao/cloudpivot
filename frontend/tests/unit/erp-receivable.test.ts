/**
 * `receivable-logic.ts` 单测（SO-15 应收台账页的纯逻辑）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里。「状态域是不是抄成了应付那套」
 * 「空筛选有没有把 `?status=` 空串发出去（后端会查出 0 行）」「核销原因按字符还是按字节」
 * 「到期当天算不算逾期」全在 `receivable-logic.ts`；`.vue` 只负责渲染，
 * 这些一旦漂移 `vue-tsc` 抓不到（编译期两边都是 `string`）。
 *
 * 断言口径全部照后端抄：
 * - 状态域 `receivable::STATUSES` / `receivable_query::STATUSES`（5 个值）；
 * - 分页键 `receivable_query::ReceivablePageQuery`（`ar_no` 模糊，**没有** `keyword`）；
 * - 核销原因 `receivable::REASON_MIN_CHARS` / `REASON_MAX_CHARS`（按字符数）；
 * - 留痕前缀 `receivable::write_off` / `cancel_for_stock_out` 写进 `remark` 的原文。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Receivable } from '@/api/business/erp';
import {
  CANCEL_REMARK_PREFIX,
  RECEIVABLE_COLUMNS,
  RECEIVABLE_PAGE_SIZE,
  RECEIVABLE_PERMS,
  RECEIVABLE_STATUS_LABELS,
  RECEIVABLE_STATUS_OPTIONS,
  RECEIPT_ALLOCATION_READY,
  WRITE_OFF_REASON_MAX,
  WRITE_OFF_REASON_MIN,
  WRITE_OFF_REMARK_PREFIX,
  agingDaysText,
  agingAsOfSourceText,
  agingPeriodStatusText,
  bucketShare,
  bucketThemeOf,
  buildReceivableQuery,
  canWriteOff,
  emptyReceivableFilter,
  hasBalance,
  isReceivableFiltered,
  normalizeWriteOffReason,
  overdueDaysOf,
  overdueText,
  receivableStatusLabel,
  receivableStatusTheme,
  receiptAllocationEmptyText,
  receivedPercent,
  settlementRecordsOf,
  sourceTypeLabel,
  writeOffBlockedReason,
  writeOffReasonError,
} from '@/views/business/erp/receivable/receivable-logic';

/** 一行真数据（形态照抄 `GET /api/business/erp/receivable/page` 返回的 list[0]） */
function row(overrides: Partial<Receivable> = {}): Receivable {
  return {
    id: '11111111-1111-1111-1111-111111111111',
    ar_no: 'AR-20261106-000001',
    customer_id: '22222222-2222-2222-2222-222222222222',
    customer_name: '北京示例客户',
    source_type: 'SALES_OUT',
    source_id: '33333333-3333-3333-3333-333333333333',
    source_no: 'SOUT-20261106-000001',
    invoice_no: null,
    invoice_date: null,
    currency_code: 'CNY',
    amount: '1130.0000',
    received_amount: '0.0000',
    unsettled_amount: '1130.0000',
    credit_days: 30,
    due_date: '2026-12-06',
    period_code: '2026-11',
    status: 'OPEN',
    status_label: '未收',
    overdue: false,
    settled_at: null,
    voucher_id: null,
    remark: null,
    created_at: '2026-11-06T02:00:00Z',
    updated_at: '2026-11-06T02:00:00Z',
    ...overrides,
  };
}

// ==================== 字段 / 常量口径 ====================

test('列定义覆盖卡面要求的 8 列 + 操作列', () => {
  const keys = RECEIVABLE_COLUMNS.map((col) => col.colKey);
  for (const required of [
    'ar_no',
    'customer_name',
    'source_no',
    'amount',
    'received_amount',
    'unsettled_amount',
    'due_date',
    'status',
  ]) {
    assert.ok(keys.includes(required), `列表缺少列 ${required}`);
  }
  assert.ok(keys.includes('op'), '列表缺少操作列');
  assert.equal(RECEIVABLE_PAGE_SIZE, 20);
});

test('权限码与 V0288 的 sys_menu.auth_code 逐字一致', () => {
  assert.deepEqual(RECEIVABLE_PERMS, {
    list: 'business:erp:receivable:list',
    detail: 'business:erp:receivable:detail',
    aging: 'business:erp:receivable:aging',
    create: 'business:erp:receivable:create',
    writeOff: 'business:erp:receivable:write-off',
  });
});

test('状态域是应收的 5 个值，不是应付那套（PARTIAL_PAID / PENDING_CANCEL 会 400）', () => {
  const values = RECEIVABLE_STATUS_OPTIONS.map((item) => item.value);
  assert.deepEqual(values, ['OPEN', 'PARTIAL', 'SETTLED', 'WRITTEN_OFF', 'CANCELLED']);
  assert.equal(values.includes('PARTIAL_PAID' as never), false);
  assert.equal(values.includes('PENDING_CANCEL' as never), false);
  // 本地中文表与下拉标签同源
  for (const item of RECEIVABLE_STATUS_OPTIONS) {
    assert.equal(item.label, RECEIVABLE_STATUS_LABELS[item.value]);
  }
});

// ==================== 筛选 → 查询参数 ====================

test('空筛选只发分页参数（空串一律不发）', () => {
  const query = buildReceivableQuery(emptyReceivableFilter(), 1);
  assert.deepEqual(query, { page: 1, page_size: RECEIVABLE_PAGE_SIZE });
});

test('筛选齐全时映射成后端键名（后台唯一模糊键是 ar_no，不是 keyword）', () => {
  const query = buildReceivableQuery(
    {
      status: 'PARTIAL',
      customerId: '22222222-2222-2222-2222-222222222222',
      arNo: '  AR-2026  ',
      sourceType: 'SALES_OUT',
      dueFrom: '2026-11-01',
      dueTo: '2026-12-31',
    },
    3,
    50,
  );
  assert.deepEqual(query, {
    page: 3,
    page_size: 50,
    status: 'PARTIAL',
    customer_id: '22222222-2222-2222-2222-222222222222',
    ar_no: 'AR-2026',
    source_type: 'SALES_OUT',
    due_date_from: '2026-11-01',
    due_date_to: '2026-12-31',
  });
  assert.equal('keyword' in query, false);
});

test('页码非法值归一到 1（后端 page 从 1 起）', () => {
  assert.equal(buildReceivableQuery(emptyReceivableFilter(), 0).page, 1);
  assert.equal(buildReceivableQuery(emptyReceivableFilter(), -5).page, 1);
  assert.equal(buildReceivableQuery(emptyReceivableFilter(), 2.9).page, 2);
});

test('isReceivableFiltered：只有空白单号不算筛选（「已筛选」徽标不误报）', () => {
  const filter = emptyReceivableFilter();
  assert.equal(isReceivableFiltered(filter), false);
  filter.arNo = '   ';
  assert.equal(isReceivableFiltered(filter), false);
  filter.arNo = 'AR-1';
  assert.equal(isReceivableFiltered(filter), true);
});

// ==================== 状态展示 ====================

test('状态中文优先用后端 status_label，缺失才回落本地表，最后回落编码', () => {
  assert.equal(receivableStatusLabel(row({ status: 'PARTIAL', status_label: '部分已收' })), '部分已收');
  assert.equal(receivableStatusLabel(row({ status: 'SETTLED', status_label: '' })), '已收清');
  assert.equal(receivableStatusLabel(row({ status: 'WEIRD', status_label: '' })), 'WEIRD');
  assert.equal(receivableStatusLabel(row({ status: '', status_label: '' })), '—');
});

test('状态配色：未知状态回落 default 不抛错', () => {
  assert.equal(receivableStatusTheme('OPEN'), 'primary');
  assert.equal(receivableStatusTheme('PARTIAL'), 'warning');
  assert.equal(receivableStatusTheme('SETTLED'), 'success');
  assert.equal(receivableStatusTheme('WRITTEN_OFF'), 'danger');
  assert.equal(receivableStatusTheme('CANCELLED'), 'default');
  assert.equal(receivableStatusTheme('???'), 'default');
});

test('来源中文：SALES_OUT / MANUAL / 未知原样', () => {
  assert.equal(sourceTypeLabel('SALES_OUT'), '销售出库');
  assert.equal(sourceTypeLabel('MANUAL'), '手工挂账');
  assert.equal(sourceTypeLabel('OTHER'), 'OTHER');
  assert.equal(sourceTypeLabel(''), '—');
});

// ==================== 坏账核销 ====================

test('只有 OPEN / PARTIAL 能核销（hasBalance 同口径）', () => {
  assert.equal(canWriteOff(row({ status: 'OPEN' })), true);
  assert.equal(canWriteOff(row({ status: 'PARTIAL' })), true);
  assert.equal(canWriteOff(row({ status: 'SETTLED' })), false);
  assert.equal(canWriteOff(row({ status: 'WRITTEN_OFF' })), false);
  assert.equal(canWriteOff(row({ status: 'CANCELLED' })), false);
  for (const status of ['OPEN', 'PARTIAL']) assert.equal(hasBalance(status), true);
  for (const status of ['SETTLED', 'WRITTEN_OFF', 'CANCELLED']) assert.equal(hasBalance(status), false);
});

test('不能核销时给出指名道姓的原因（按钮灰了也要说明为什么）', () => {
  assert.equal(writeOffBlockedReason(row({ status: 'OPEN', status_label: '未收' })), '');
  assert.match(writeOffBlockedReason(row({ status: 'SETTLED', status_label: '已收清' })), /钱已经收齐/);
  assert.match(writeOffBlockedReason(row({ status: 'WRITTEN_OFF', status_label: '坏账核销' })), /已经核销过/);
  assert.match(writeOffBlockedReason(row({ status: 'CANCELLED', status_label: '已作废' })), /出库单已作废/);
  assert.match(writeOffBlockedReason(row({ status: 'X', status_label: '怪状态' })), /怪状态/);
});

test('核销原因按字符数判 4..500（中文 4 个字要能过，英文 3 个字母不过）', () => {
  assert.equal(WRITE_OFF_REASON_MIN, 4);
  assert.equal(WRITE_OFF_REASON_MAX, 500);
  assert.equal(writeOffReasonError(''), '坏账核销必须给出原因（会计估计变更，没原因无法审计）');
  assert.equal(writeOffReasonError('   '), '坏账核销必须给出原因（会计估计变更，没原因无法审计）');
  assert.notEqual(writeOffReasonError('abc'), '', '3 个字符应被拒');
  assert.equal(writeOffReasonError('客户注销'), '', '4 个中文字符应通过（按字符不按字节）');
  assert.equal(writeOffReasonError('客户已注销经理批准'), '');
  assert.match(writeOffReasonError('x'.repeat(WRITE_OFF_REASON_MAX + 1)), /不能超过 500/);
  assert.equal(writeOffReasonError('x'.repeat(WRITE_OFF_REASON_MAX)), '');
  assert.equal(writeOffReasonError(null), '坏账核销必须给出原因（会计估计变更，没原因无法审计）');
});

test('原因归一：去首尾空白 + 连续空白折一个空格（与后端收到的字符串一致）', () => {
  assert.equal(normalizeWriteOffReason('  客户\n\n已   注销  '), '客户 已 注销');
});

// ==================== 逾期 / 账龄天数 ====================

test('逾期天数：正数逾期 / 0 到期当天 / 负数未到期', () => {
  assert.equal(overdueDaysOf('2026-11-01', '2026-11-06'), 5);
  assert.equal(overdueDaysOf('2026-11-06', '2026-11-06'), 0);
  assert.equal(overdueDaysOf('2026-12-06', '2026-11-06'), -30);
  // 跨年、跨月都要对（不能用 Date.parse 的时区漂移）
  assert.equal(overdueDaysOf('2025-12-31', '2026-01-01'), 1);
  assert.equal(overdueDaysOf('2026-02-28', '2026-03-01'), 1);
  // 非法输入不抛错
  assert.equal(overdueDaysOf('', '2026-11-06'), 0);
  assert.equal(overdueDaysOf('not-a-date', '2026-11-06'), 0);
});

test('逾期文案：只有 OPEN/PARTIAL 才提逾期（已结清的单不该挂红标）', () => {
  const today = '2026-11-06';
  assert.equal(overdueText(row({ status: 'OPEN', due_date: '2026-10-01' }), today), '逾期 36 天');
  assert.equal(overdueText(row({ status: 'PARTIAL', due_date: '2026-11-06' }), today), '今天到期');
  assert.equal(overdueText(row({ status: 'OPEN', due_date: '2026-12-06' }), today), '');
  assert.equal(overdueText(row({ status: 'SETTLED', due_date: '2026-10-01' }), today), '');
  assert.equal(overdueText(row({ status: 'CANCELLED', due_date: '2026-10-01' }), today), '');
});

test('账龄天数文案与应付账龄页同一套说法', () => {
  assert.equal(agingDaysText(36), '逾期 36');
  assert.equal(agingDaysText(0), '到期当天');
  assert.equal(agingDaysText(-7), '未到期 7');
});

// ==================== 金额比例 ====================

test('已收比例：整数分口径，超额截到 100%', () => {
  assert.equal(receivedPercent('1130.0000', '0.0000'), 0);
  assert.equal(receivedPercent('1000', '500'), 50);
  assert.equal(receivedPercent('3', '1'), 33.3);
  assert.equal(receivedPercent('1000', '2000'), 100);
  assert.equal(receivedPercent('0', '0'), 0);
});

test('账龄格占比：合计为 0 或格为 0 一律 0（不产生 NaN 宽度）', () => {
  assert.equal(bucketShare('300', '1000'), 30);
  assert.equal(bucketShare('0', '1000'), 0);
  assert.equal(bucketShare('100', '0'), 0);
  assert.equal(bucketShare(null, '1000'), 0);
  assert.equal(bucketShare('300', null), 0);
  assert.equal(bucketShare('1200', '1000'), 100);
});

// ==================== 账龄看板文案 ====================

test('账龄档位配色：未到期绿 / 90+ 红 / 未知灰', () => {
  assert.equal(bucketThemeOf('NOT_DUE'), 'success');
  assert.equal(bucketThemeOf('D0_30'), 'primary');
  assert.equal(bucketThemeOf('D31_60'), 'warning');
  assert.equal(bucketThemeOf('D61_90'), 'warning');
  assert.equal(bucketThemeOf('OVER_90'), 'danger');
  assert.equal(bucketThemeOf('D180_PLUS'), 'default');
});

test('基准日来源与期间状态文案', () => {
  assert.equal(agingAsOfSourceText('QUERY'), '手动指定');
  assert.equal(agingAsOfSourceText('PERIOD'), '期间末日');
  assert.equal(agingAsOfSourceText('TODAY'), '今天');
  assert.equal(agingAsOfSourceText(''), '—');
  assert.equal(agingPeriodStatusText('CLOSED'), '已结账');
  assert.equal(agingPeriodStatusText('OPEN'), '未结账');
  assert.equal(agingPeriodStatusText('DERIVED'), '未开账·按自然月推算');
  assert.equal(agingPeriodStatusText(null), '');
});

// ==================== 详情「核销记录」 ====================

test('留痕前缀与后端写进 remark 的原文一致', () => {
  assert.equal(WRITE_OFF_REMARK_PREFIX, '[坏账核销]');
  assert.equal(CANCEL_REMARK_PREFIX, '[出库作废冲销]');
});

test('坏账核销留痕：从 remark 拆出原因，时间用 settled_at', () => {
  const records = settlementRecordsOf(
    row({
      status: 'WRITTEN_OFF',
      remark: '[坏账核销] 客户已注销，经理批准核销',
      settled_at: '2026-11-06T03:04:05Z',
    }),
  );
  assert.equal(records.length, 1);
  assert.equal(records[0].kind, 'WRITE_OFF');
  assert.equal(records[0].title, '坏账核销');
  assert.equal(records[0].reason, '客户已注销，经理批准核销');
  assert.equal(records[0].time, '2026-11-06T03:04:05Z');
});

test('出库作废冲销留痕：与坏账核销同一面板但 kind 不同', () => {
  const records = settlementRecordsOf(
    row({
      status: 'CANCELLED',
      remark: '[出库作废冲销] 出库单 SOUT-1 作废',
      settled_at: '2026-11-06T03:04:05Z',
    }),
  );
  assert.equal(records.length, 1);
  assert.equal(records[0].kind, 'CANCEL');
  assert.equal(records[0].reason, '出库单 SOUT-1 作废');
});

test('多条留痕按行拆开（后端一行一条），无关备注行被忽略', () => {
  const records = settlementRecordsOf(
    row({
      status: 'CANCELLED',
      remark: '业务备注\n[坏账核销] 甲\n[出库作废冲销] 乙\n',
      settled_at: null,
    }),
  );
  assert.deepEqual(
    records.map((item) => [item.kind, item.reason, item.time]),
    [
      ['WRITE_OFF', '甲', null],
      ['CANCEL', '乙', null],
    ],
  );
});

test('没有留痕的普通单返回空数组（不拿「已收 > 0」编收款行）', () => {
  assert.deepEqual(settlementRecordsOf(row({ status: 'OPEN', remark: null })), []);
  assert.deepEqual(settlementRecordsOf(row({ status: 'PARTIAL', remark: '   ' })), []);
  assert.deepEqual(settlementRecordsOf(row({ status: 'SETTLED', remark: '普通备注' })), []);
});

test('收款核销未上线：空态文案要据实说明，且已收 > 0 时点名原因', () => {
  assert.equal(RECEIPT_ALLOCATION_READY, false);
  assert.match(receiptAllocationEmptyText('0'), /尚未上线/);
  assert.match(receiptAllocationEmptyText('500.00'), /已收金额不为 0/);
  assert.match(receiptAllocationEmptyText('500.00'), /SO-17/);
});
