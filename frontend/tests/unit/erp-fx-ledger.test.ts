/**
 * 外币明细账的纯逻辑单测（M1-08 · V0413）。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「金额格式化」「余额算借还是贷」「区间怎么显示」
 * 「空表怎么说」全在 `fx-ledger-logic.ts`；`.vue` 只负责画。这些口径漂移时
 * `vue-tsc` 抓不到（比如有人把 `balanceText` 换成 `formatAmount` 就会丢掉「借/贷」方向，
 * 再有人补个 `Math.abs()` 就把贷方余额画成借方），只有写成函数才能钉住。
 *
 * 断言口径与后端逐字对齐：
 * - 余额是有符号值（借方为正），负数 = 贷方（`fx_ledger::side_of`）
 * - 余额**按科目分别累计**，含该科目期初（页面必须显示科目列）
 * - 本位币三列**不含**历次调汇（含调汇看外币余额表）
 * - 权限码 `business:erp:report:fx-ledger`（V0413 的 `sys_menu.auth_code`）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  EMPTY_PLACEHOLDER,
  FX_LEDGER_REPORT_CODE,
  PERM_FX_LEDGER,
  RED_LABEL,
  amountCell,
  balanceText,
  bookColumnHint,
  closingTotalText,
  currencyLabel,
  emptyText,
  formatAmount,
  rangeText,
  rateText,
  runningColumnHint,
  sideLabel,
  sideOf,
} from '@/views/business/erp/report/fx-ledger/fx-ledger-logic';

test('权限码 / 报表编码与迁移与后端逐字一致', () => {
  // 报表页码就是接口码（ERP 侧尚未做 page/list 解耦，见编码规范 §1.7.2 过渡期条款）
  assert.equal(PERM_FX_LEDGER, 'business:erp:report:fx-ledger');
  assert.equal(FX_LEDGER_REPORT_CODE, 'FX_LEDGER');
});

test('金额格式化：十进制字符串四舍五入到分，不走 Number()', () => {
  assert.equal(formatAmount('1234.5650'), '1,234.57');
  assert.equal(formatAmount('100.0000'), '100.00');
  assert.equal(formatAmount('-300.0000'), '-300.00');
  assert.equal(formatAmount('0.0000'), '0.00');
  // 二进制浮点会给出 0.59（Number('0.595').toFixed(2)），报表上少一分是事故
  assert.equal(formatAmount('0.5950'), '0.60');
  // 进位串到高位
  assert.equal(formatAmount('999.9950'), '1,000.00');
  // 空值不给数字（交给 amountCell 显示占位符）
  assert.equal(formatAmount(null), '');
  assert.equal(formatAmount(undefined), '');
  assert.equal(formatAmount(''), '');
});

test('空值一律显示占位符，绝不显示 0', () => {
  assert.equal(amountCell(null), EMPTY_PLACEHOLDER);
  assert.equal(amountCell(''), EMPTY_PLACEHOLDER);
  assert.equal(amountCell('0.0000'), '0.00', '真的 0 要显示 0.00（不是缺数据）');
});

test('余额方向：负数 = 贷方（有符号口径，不取绝对值）', () => {
  assert.equal(sideOf('300.0000'), 'DEBIT');
  assert.equal(sideOf('-300.0000'), 'CREDIT');
  assert.equal(sideOf('0.0000'), 'ZERO');
  assert.equal(sideOf('-0.0000'), 'ZERO', '-0 是平，不是贷方');
  assert.equal(sideOf(null), 'ZERO');
  assert.equal(sideOf(''), 'ZERO');

  assert.equal(sideLabel('DEBIT'), '借');
  assert.equal(sideLabel('CREDIT'), '贷');
  assert.equal(sideLabel('ZERO'), '平');

  assert.equal(balanceText('300.0000'), '借 300.00');
  assert.equal(balanceText('-300.0000'), '贷 300.00');
  assert.equal(balanceText('0.0000'), '平');
});

test('期末净头寸：按方向显示，零为「平」', () => {
  assert.equal(closingTotalText({ closing_fc: '300.0000', closing_side: 'DEBIT' }), '借 300.00');
  assert.equal(closingTotalText({ closing_fc: '-300.0000', closing_side: 'CREDIT' }), '贷 300.00');
  assert.equal(closingTotalText({ closing_fc: '0.0000', closing_side: 'ZERO' }), '平');
});

test('汇率按 8 位小数显示，缺失显示占位符', () => {
  assert.equal(rateText('7.2'), '7.20000000');
  assert.equal(rateText('7.12345678'), '7.12345678');
  assert.equal(rateText(null), EMPTY_PLACEHOLDER);
  assert.equal(rateText(''), EMPTY_PLACEHOLDER);
});

test('区间文字覆盖「双端 / 只有起 / 只有止 / 不限」四种形态', () => {
  assert.equal(rangeText({ date_from: '2026-11-01', date_to: '2026-11-30' }), '2026-11-01 ~ 2026-11-30');
  assert.equal(rangeText({ date_from: '2026-11-01', date_to: null }), '2026-11-01 起');
  assert.equal(rangeText({ date_from: null, date_to: '2026-11-30' }), '截至 2026-11-30');
  assert.equal(rangeText({ date_from: null, date_to: null }), '全部历史');
});

test('本位币列与逐行余额的说明必须点明「不含调汇」「按科目累计」', () => {
  assert.ok(bookColumnHint('USD', 'CNY').includes('调汇'));
  assert.ok(bookColumnHint('USD', 'CNY').includes('外币余额表'));
  assert.ok(runningColumnHint().includes('按科目'));
  assert.ok(runningColumnHint().includes('期初'));
});

test('币种显示名 / 空表提示 / 红字标记', () => {
  assert.equal(currencyLabel('USD', '美元'), 'USD 美元');
  assert.equal(currencyLabel('USD', null), 'USD');
  assert.equal(RED_LABEL, '红字');

  // 全量（无科目过滤）
  const all = emptyText({ currency: 'USD', account: null, date_from: '2026-11-01', date_to: '2026-11-30' });
  assert.ok(all.includes('USD'));
  assert.ok(all.includes('2026-11-01'));
  assert.ok(all.includes('不是出错'));

  // 筛了科目
  const one = emptyText({
    currency: 'USD',
    account: { id: 'x', account_code: '1001', account_name: '库存现金', is_foreign_currency: true },
    date_from: null,
    date_to: null,
  });
  assert.ok(one.includes('1001'));
  assert.ok(one.includes('全部历史'));

  // 筛了**未标记外币核算**的科目：要说清原因与怎么修，不能只说「没有数据」
  const unmarked = emptyText({
    currency: 'USD',
    account: { id: 'x', account_code: '1002', account_name: '银行存款', is_foreign_currency: false },
    date_from: null,
    date_to: null,
  });
  assert.ok(unmarked.includes('未标记'));
  assert.ok(unmarked.includes('外币核算'));
});
