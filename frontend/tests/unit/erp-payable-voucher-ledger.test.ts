/**
 * 应付页 + 凭证页 + 总账页纯逻辑单测（P2-E2）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里，而这三页里
 * 「状态是橙还是蓝」「Σ借 是不是等于 Σ贷」「期初 + 借 − 贷 是不是期末」
 * 恰恰是会误导财务的判定 —— 全部搬进 `*-logic.ts`，`.vue` 只负责渲染。
 *
 * 断言口径照后端抄：
 * - 应付状态 = `backend/src/business/erp/payable.rs` 的 `STATUS_*` 四个常量；
 * - 凭证状态 = `voucher_query.rs::STATUSES` + `status_label` 文案；
 * - 平衡 = `biz_voucher` 的 `CHECK (total_debit = total_credit)` / `66009`；
 * - 余额恒等式 = `ledger.rs` 文件头 `closing_net = opening_net + Σ本期借 − Σ本期贷`。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  PAYABLE_STATUS_FILTER_OPTIONS,
  buildPayableQuery,
  emptyPayableFilter,
  isOverdue,
  paidPercent,
  payableStatusLabel,
  payableStatusTheme,
  sourceText,
  unpaidOf,
} from '@/views/business/erp/payable/payable-logic';
import {
  VOUCHER_STATUS_FILTER_OPTIONS,
  balanceCheck,
  balanceText,
  buildVoucherQuery,
  emptyVoucherFilter,
  isBalanced,
  rowBalanced,
  sourceTypeLabel,
  sumEntries,
  voucherStatusLabel,
  voucherStatusTheme,
  voucherTypeLabel,
} from '@/views/business/erp/voucher/voucher-logic';
import {
  buildLedgerQuery,
  checkTrialBalanceRow,
  emptyLedgerFilter,
  ledgerPageTotals,
  ledgerRangeError,
  runningBreaks,
  trialBalancePeriodError,
  trialBalanceTotals,
  trialBalanceVerdict,
} from '@/views/business/erp/ledger/ledger-logic';
import { formatCents, formatMoney, toCents } from '@/views/business/erp/shared/money';
import type { TrialBalanceRow } from '@/api/business/erp';

/** 造一行余额表数据（默认全 0，只覆盖要断言的列） */
function trialRow(overrides: Partial<TrialBalanceRow>): TrialBalanceRow {
  return {
    account_id: 'acc',
    account_code: '1405',
    account_name: '库存商品',
    account_category: 'ASSET',
    balance_direction: 'DEBIT',
    opening_debit: 0,
    opening_credit: 0,
    period_debit: 0,
    period_credit: 0,
    closing_debit: 0,
    closing_credit: 0,
    ...overrides,
  };
}

// ==================== ① 状态映射 ====================

test('① 应付 4 态 → 中文 + 颜色（橙/蓝/绿/灰），未知状态原样返回', () => {
  // 文案对齐 `payable.rs`：OPEN=未付 / PARTIAL_PAID=部分付款 / SETTLED=已结清 / CANCELLED=已作废
  assert.equal(payableStatusLabel('OPEN'), '未付');
  assert.equal(payableStatusLabel('PARTIAL_PAID'), '部分付款');
  assert.equal(payableStatusLabel('SETTLED'), '已结清');
  assert.equal(payableStatusLabel('CANCELLED'), '已作废');

  // 卡面 §三 的配色：橙(warning) / 蓝(primary) / 绿(success) / 灰(default)
  assert.equal(payableStatusTheme('OPEN'), 'warning');
  assert.equal(payableStatusTheme('PARTIAL_PAID'), 'primary');
  assert.equal(payableStatusTheme('SETTLED'), 'success');
  assert.equal(payableStatusTheme('CANCELLED'), 'default');

  // 后端加了新状态、前端没跟上时：不许吞成空白（空标签比英文原样更难查）
  assert.equal(payableStatusLabel('REFUNDED'), 'REFUNDED');
  assert.equal(payableStatusTheme('REFUNDED'), 'default');

  // 筛选下拉 6 项：全部 + 4 态 + P-FLOW-3B 的「作废审批中」，顺序与卡面一致
  assert.deepEqual(
    PAYABLE_STATUS_FILTER_OPTIONS.map((item) => item.value),
    ['', 'OPEN', 'PARTIAL_PAID', 'SETTLED', 'PENDING_CANCEL', 'CANCELLED'],
  );
});

test('① 凭证 4 态 → 中文 + 颜色，来源/类型标签与后端逐字一致', () => {
  // 文案对齐 `voucher_query::status_label`
  assert.equal(voucherStatusLabel('POSTED'), '已记账');
  assert.equal(voucherStatusLabel('DRAFT'), '草稿');
  assert.equal(voucherStatusLabel('AUDITED'), '已审核');
  assert.equal(voucherStatusLabel('CANCELLED'), '已作废');

  // POSTED 是唯一进总账的状态 → 绿；DRAFT 灰；AUDITED 蓝；CANCELLED 红
  assert.equal(voucherStatusTheme('POSTED'), 'success');
  assert.equal(voucherStatusTheme('DRAFT'), 'default');
  assert.equal(voucherStatusTheme('AUDITED'), 'primary');
  assert.equal(voucherStatusTheme('CANCELLED'), 'danger');
  assert.equal(voucherStatusLabel('REVERSED'), 'REVERSED');

  // 类型 / 来源（卡面写「采购入库」，真实值是 STOCK_IN）
  assert.equal(voucherTypeLabel('AUTO'), '自动');
  assert.equal(voucherTypeLabel('MANUAL'), '手工');
  assert.equal(sourceTypeLabel('STOCK_IN'), '采购入库');
  assert.equal(sourceTypeLabel('MANUAL'), '手工');
  assert.equal(sourceTypeLabel(null), '—');

  assert.deepEqual(
    VOUCHER_STATUS_FILTER_OPTIONS.map((item) => item.value),
    ['', 'POSTED', 'DRAFT', 'AUDITED', 'CANCELLED'],
  );
});

// ==================== ② 借贷平衡判断 ====================

test('② 借贷平衡：Σ借 == Σ贷 → true，不等 → false（含后端 Decimal 字符串与浮点陷阱）', () => {
  // 卡面 §四 的三行分录：200 + 26 借 / 226 贷
  const entries = [
    { debit_amount: '200.0000', credit_amount: '0.0000' },
    { debit_amount: '26.0000', credit_amount: '0.0000' },
    { debit_amount: '0.0000', credit_amount: '226.0000' },
  ];
  const totals = sumEntries(entries);
  assert.equal(totals.debitCents, 22600);
  assert.equal(totals.creditCents, 22600);
  assert.equal(totals.diffCents, 0);
  assert.equal(totals.balanced, true);
  assert.equal(isBalanced(entries), true);

  // 不平：贷方少一分
  const broken = [
    { debit_amount: 200, credit_amount: 0 },
    { debit_amount: 26, credit_amount: 0 },
    { debit_amount: 0, credit_amount: 225.99 },
  ];
  assert.equal(isBalanced(broken), false);
  assert.equal(sumEntries(broken).diffCents, 1);
  assert.equal(balanceText(false, 1), '借贷不平，差额 0.01');
  assert.equal(balanceText(true), '借贷平衡');

  // 浮点陷阱：0.1 + 0.2 !== 0.3，但整数分下必须相等（否则财务页天天假报警）
  assert.equal(toCents(0.1) + toCents(0.2), toCents(0.3));
  assert.equal(
    isBalanced([
      { debit_amount: '0.1000', credit_amount: '0.1000' },
      { debit_amount: '0.2000', credit_amount: '0.2000' },
      { debit_amount: 0, credit_amount: 0 },
    ]),
    true,
  );

  // 红字凭证（P2-C1：借贷互换、金额恒正）—— 单行净额为负，但整张凭证仍然平
  const red = [
    { debit_amount: '226.0000', credit_amount: 0 },
    { debit_amount: 0, credit_amount: '200.0000' },
    { debit_amount: 0, credit_amount: '26.0000' },
  ];
  assert.equal(isBalanced(red), true);
  assert.equal(sumEntries(red).diffCents, 0);

  // 空分录（详情还没加载完）不许判成「不平」，否则抽屉会闪一下红字
  assert.equal(isBalanced([]), true);
  assert.equal(sumEntries([]).diffCents, 0);

  // 列表行只比单头两列
  assert.equal(rowBalanced({ total_debit: '226.0000', total_credit: '226.0000' }), true);
  assert.equal(rowBalanced({ total_debit: '226.0000', total_credit: '226.01' }), false);
});

test('② 详情三方校验：前端逐行加 / 后端分录合计 / 单头合计 三者一致才算「真的平」', () => {
  const detail = {
    entries: [
      { debit_amount: '200.0000', credit_amount: '0.0000' },
      { debit_amount: '26.0000', credit_amount: '0.0000' },
      { debit_amount: '0.0000', credit_amount: '226.0000' },
    ],
    entry_total_debit: '226.0000',
    entry_total_credit: '226.0000',
    total_debit: '226.0000',
    total_credit: '226.0000',
  };
  const ok = balanceCheck(detail);
  assert.equal(ok.balanced, true);
  assert.equal(ok.consistent, true);
  assert.equal(ok.mismatch, '');

  // 响应自相矛盾：分录逐行加 226，却报分录合计 225 —— 后端 66009 的闸没兜住
  const bad = balanceCheck({ ...detail, entry_total_debit: '225.0000' });
  assert.equal(bad.consistent, false);
  assert.equal(bad.mismatch.includes('不一致'), true);
});

// ==================== ③ 余额计算 ====================

test('③ 余额恒等式：期初 + 本期借 − 本期贷 = 期末（逐行 + 合计 + 平衡判定）', () => {
  // 卡面 §五 Tab2 的三行数据（期初 0）
  const rows = [
    trialRow({ account_code: '1405', period_debit: '200.00', closing_debit: '200.00' }),
    trialRow({
      account_code: '2202',
      account_name: '应付账款',
      balance_direction: 'CREDIT',
      period_credit: '226.00',
      closing_credit: '226.00',
    }),
    trialRow({
      account_code: '2221.01',
      account_name: '应交税费-进项税',
      period_debit: '26.00',
      closing_debit: '26.00',
    }),
  ];

  for (const row of rows) {
    const check = checkTrialBalanceRow(row);
    assert.equal(check.ok, true, `${row.account_code} 恒等式不成立`);
    assert.equal(check.openingNetCents + check.periodNetCents, check.closingNetCents);
  }

  // 2202 是贷方科目：期末净额 = −226.00（卡面也画成 −226.00）
  assert.equal(checkTrialBalanceRow(rows[1]!).closingNetCents, -22600);

  const totals = trialBalanceTotals(rows);
  assert.equal(totals.count, 3);
  assert.equal(totals.periodDebitCents, 22600);
  assert.equal(totals.periodCreditCents, 22600);
  assert.equal(totals.closingDebitCents, 22600);
  assert.equal(totals.closingCreditCents, 22600);
  assert.equal(totals.periodBalanced, true);
  assert.equal(totals.closingBalanced, true);
  assert.equal(totals.balanced, true);
  assert.deepEqual(totals.brokenRows, []);
  assert.equal(trialBalanceVerdict(totals), '平衡');

  // 期初不为 0 也要成立：借 100 → 期中借 50 贷 20 → 期末 130
  const carried = trialRow({
    opening_debit: '100.00',
    period_debit: '50.00',
    period_credit: '20.00',
    closing_debit: '130.00',
  });
  assert.equal(checkTrialBalanceRow(carried).ok, true);
  assert.equal(checkTrialBalanceRow(carried).closingNetCents, 13000);

  // 期末算错 → 该行被点名，总判定为 false
  const wrong = trialBalanceTotals([...rows, trialRow({ account_code: '9999', period_debit: '10.00', closing_debit: '9.00' })]);
  assert.equal(wrong.balanced, false);
  assert.deepEqual(wrong.brokenRows, ['9999']);
  assert.equal(wrong.periodBalanced, false);
  assert.equal(trialBalanceVerdict(wrong).startsWith('本期不平'), true);

  // 本期借贷不平（脏数据 / 后端 66009 没拦住）→ 红字文案带差额
  const unbalanced = trialBalanceTotals([
    trialRow({ account_code: '1405', period_debit: '200.00', closing_debit: '200.00' }),
    trialRow({ account_code: '2202', period_credit: '199.00', closing_credit: '199.00' }),
  ]);
  assert.equal(unbalanced.periodBalanced, false);
  assert.equal(unbalanced.balanced, false);
  assert.equal(trialBalanceVerdict(unbalanced), '本期不平（差 1.00）');
});

test('③ 流水逐行余额自洽：第 i 行 = 第 i−1 行 + 本行净额', () => {
  const ok = [
    { debit_amount: '200.00', credit_amount: '0.00', running_balance: '200.00' },
    { debit_amount: '26.00', credit_amount: '0.00', running_balance: '226.00' },
    { debit_amount: '0.00', credit_amount: '226.00', running_balance: '0.00' },
  ];
  assert.deepEqual(runningBreaks(ok), []);
  assert.equal(ledgerPageTotals(ok).netCents, 0);
  assert.equal(ledgerPageTotals(ok).count, 3);

  // 第 3 行余额算错（后端前缀和 bug / 脏数据）
  const broken = [
    { debit_amount: '200.00', credit_amount: '0.00', running_balance: '200.00' },
    { debit_amount: '26.00', credit_amount: '0.00', running_balance: '226.00' },
    { debit_amount: '0.00', credit_amount: '226.00', running_balance: '1.00' },
  ];
  assert.deepEqual(runningBreaks(broken), [2]);

  // 翻页：第 2 页第一行含期初（本页之前净额 999），带 startCents 时第一行也要对
  const page2 = [
    { debit_amount: '10.00', credit_amount: '0.00', running_balance: '1009.00' },
    { debit_amount: '0.00', credit_amount: '9.00', running_balance: '1000.00' },
  ];
  assert.deepEqual(runningBreaks(page2, 99900), []);
  assert.deepEqual(runningBreaks(page2, 0), [0, 1]);
});

// ==================== ④ 查询参数 / 边界 ====================

test('④ 查询参数：空串一律不发，科目/期间缺失时在界面上拦住', () => {
  // 应付
  assert.deepEqual(buildPayableQuery(emptyPayableFilter(), 1, 20), { page: 1, page_size: 20 });
  assert.deepEqual(
    buildPayableQuery({ ...emptyPayableFilter(), status: 'OPEN', payable_no: '  AP-20261031  ' }, 2, 20),
    { page: 2, page_size: 20, status: 'OPEN', payable_no: 'AP-20261031' },
  );

  // 凭证：期间不填 = 全部期间（会计核历史时不该被默认当月挡住）
  assert.deepEqual(buildVoucherQuery(emptyVoucherFilter(), 1, 20), { page: 1, page_size: 20 });
  assert.deepEqual(
    buildVoucherQuery({ ...emptyVoucherFilter(), period: ' 2026-10 ', status: 'POSTED' }, 1, 20),
    { page: 1, page_size: 20, period: '2026-10', status: 'POSTED' },
  );

  // 总账：没选科目 → null（后端 account_id/account_code 二选一必填，发了必然 400）
  assert.equal(buildLedgerQuery(emptyLedgerFilter(), 1), null);
  assert.deepEqual(buildLedgerQuery({ ...emptyLedgerFilter(), account_id: 'a1' }, 1, 20), {
    account_id: 'a1',
    page: 1,
    page_size: 20,
  });
  assert.equal(ledgerRangeError({ period_from: '2026-11', period_to: '2026-10' }), '起始期间不能晚于结束期间');
  assert.equal(ledgerRangeError({ period_from: '2026-10', period_to: '2026-10' }), '');

  // 余额表：期间必填且必须是 yyyy-MM（后端 `is_month_key` 同口径）
  assert.equal(trialBalancePeriodError(''), '请选择会计期间（yyyy-MM）');
  assert.equal(trialBalancePeriodError('2026-13').includes('yyyy-MM'), true);
  assert.equal(trialBalancePeriodError('2026-10'), '');
});

test('④ 应付页辅助判定：逾期 / 付款进度 / 未付兜底 / 来源文案', () => {
  // 未付 + 到期日已过 → 逾期；已结清 / 已作废不标
  assert.equal(isOverdue({ status: 'OPEN', due_date: '2026-10-01' }, '2026-10-31'), true);
  assert.equal(isOverdue({ status: 'PARTIAL_PAID', due_date: '2026-10-01' }, '2026-10-31'), true);
  assert.equal(isOverdue({ status: 'SETTLED', due_date: '2026-10-01' }, '2026-10-31'), false);
  assert.equal(isOverdue({ status: 'OPEN', due_date: '2026-12-01' }, '2026-10-31'), false);

  // 付款进度用整数分算（226 付 113 → 50%）
  assert.equal(paidPercent('113.0000', '226.0000'), 50);
  assert.equal(paidPercent(0, '226.0000'), 0);
  assert.equal(paidPercent('5', '0'), 0);

  // 未付优先读库列；缺列才 amount − paid 兜底
  assert.equal(unpaidOf({ amount: '226.0000', paid_amount: '0.0000', unpaid_amount: '226.0000' }), 226);
  assert.equal(unpaidOf({ amount: 226, paid_amount: 26, unpaid_amount: null }), 200);

  // 来源文案：STOCK_IN + 收货单号
  assert.equal(sourceText({ source_type: 'STOCK_IN', source_no: 'IN-20261031-000001' }), '收货入库 IN-20261031-000001');
});

test('④ 金额格式化：整数分、千分位、负数（不出现 -0.00）', () => {
  assert.equal(formatCents(22600), '226.00');
  assert.equal(formatCents(1234560), '12,345.60');
  assert.equal(formatCents(-22600), '-226.00');
  assert.equal(formatCents(0), '0.00');
  assert.equal(formatMoney('226.0000'), '226.00');
  assert.equal(formatMoney(null), '0.00');
  // toCents 的负数中点：-0.5 分不存在，但 -1.005 元这类要按绝对值舍入
  assert.equal(toCents('-1.005'), -101);
  assert.equal(toCents('-0.001'), 0);
});
