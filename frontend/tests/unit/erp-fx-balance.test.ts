/**
 * 外币余额表的纯逻辑单测（M1-07 · V0412）。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「金额格式化」「余额算借还是贷」「缺牌价显示什么」
 * 「合计能不能显示」全在 `fx-balance-logic.ts`；`.vue` 只负责画。这些口径漂移时
 * `vue-tsc` 抓不到（比如有人把 `amountCell` 换成 `formatAmount` 就会把 `null` 画成空串，
 * 再有人补个 `|| '0.00'` 就变成「折算是 0 元」），只有写成函数才能钉住。
 *
 * 断言口径与后端逐字对齐：
 * - 余额是有符号值（借方为正），负数 = 贷方（`fx_balance::side_of`）
 * - 缺牌价 `converted_base` / `reval_diff` = `null`，**没有**按 rate = 1 折算（模块文档 ③）
 * - `total_converted_base` 在任一币种缺牌价时 = `null`（半套折算的合计不给）
 * - 权限码 `business:erp:report:fx-balance`（V0412 的 `sys_menu.auth_code`）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { PERM_FX_BALANCE, FX_BALANCE_REPORT_CODE } from '@/views/business/erp/report/fx-balance/fx-balance-logic';
import {
  MISSING_RATE_PLACEHOLDER,
  amountCell,
  balanceText,
  basisHint,
  basisLabel,
  convertedTotalText,
  currencyLabel,
  diffText,
  diffTotalText,
  emptyText,
  formatAmount,
  missingRateText,
  periodStatusText,
  rateText,
  sideOf,
} from '@/views/business/erp/report/fx-balance/fx-balance-logic';

test('权限码 / 报表编码与迁移与后端逐字一致', () => {
  assert.equal(PERM_FX_BALANCE, 'business:erp:report:fx-balance');
  assert.equal(FX_BALANCE_REPORT_CODE, 'FX_BALANCE');
  // 报表页码就是接口码（ERP 侧尚未做 page/list 解耦，见编码规范 §1.7.2 过渡期条款）
  assert.equal(PERM_FX_BALANCE, 'business:erp:report:fx-balance');
});

test('金额格式化：十进制字符串四舍五入到分，不走 Number()', () => {
  assert.equal(formatAmount('1234.5650'), '1,234.57');
  assert.equal(formatAmount('1234567.8912'), '1,234,567.89');
  assert.equal(formatAmount('-2120.0000'), '-2,120.00');
  assert.equal(formatAmount('0.0000'), '0.00');
  // 二进制浮点会给出 0.59（Number('0.595').toFixed(2)），报表上少一分是事故
  assert.equal(formatAmount('0.5950'), '0.60');
  // 进位串到高位（999.995 → 1000.00）
  assert.equal(formatAmount('999.9950'), '1,000.00');
  // 空值不给数字（缺牌价的折算列交给 amountCell 显示占位符）
  assert.equal(formatAmount(null), '');
  assert.equal(formatAmount(undefined), '');
  assert.equal(formatAmount(''), '');
});

test('缺牌价一律显示占位符，绝不显示 0（null 是「没算」不是「算出来是 0」）', () => {
  assert.equal(amountCell(null), MISSING_RATE_PLACEHOLDER);
  assert.equal(amountCell(undefined), MISSING_RATE_PLACEHOLDER);
  assert.equal(amountCell(''), MISSING_RATE_PLACEHOLDER);
  assert.equal(amountCell('2370.0000'), '2,370.00');
  // 真的 0 要显示 0.00（仓位冲平的折算值是 0，不是缺数据）
  assert.equal(amountCell('0.0000'), '0.00');
});

test('余额方向：负数 = 贷方（有符号口径，不取绝对值）', () => {
  assert.equal(sideOf('2120.0000'), 'DEBIT');
  assert.equal(sideOf('-2070.0000'), 'CREDIT');
  assert.equal(sideOf('0.0000'), 'ZERO');
  assert.equal(sideOf('-0.0000'), 'ZERO', '-0 是平，不是贷方');
  assert.equal(sideOf(null), 'ZERO');
  assert.equal(sideOf(''), 'ZERO');

  assert.equal(balanceText('2120.0000'), '借 2,120.00');
  assert.equal(balanceText('-2070.0000'), '贷 2,070.00');
  assert.equal(balanceText('0.0000'), '平');
});

test('汇兑差额的文字口径：正 = 调增、负 = 调减、缺牌价 = 占位符', () => {
  assert.equal(diffText('250.0000'), '调增 250.00');
  assert.equal(diffText('-300.0000'), '调减 300.00');
  assert.equal(diffText('0.0000'), '0.00');
  assert.equal(diffText(null), MISSING_RATE_PLACEHOLDER);
});

test('汇率按 8 位小数显示，缺牌价显示占位符', () => {
  assert.equal(rateText('7.9'), '7.90000000');
  assert.equal(rateText('7.12345678'), '7.12345678');
  assert.equal(rateText(null), MISSING_RATE_PLACEHOLDER);
});

test('合计区：任一币种缺牌价时折算与差额合计都不显示', () => {
  const full = { total_converted_base: '2370.0000', total_reval_diff: '250.0000' };
  assert.equal(convertedTotalText({ total_converted_base: full.total_converted_base, missing_rates: [] }), '2,370.00');
  assert.equal(diffTotalText(full), '250.00');

  // 半套折算的合计会被当成真数抄走 ⇒ 后端给 null，前端必须显示占位符
  const partial = { total_converted_base: null, missing_rates: ['EUR'] };
  assert.equal(convertedTotalText(partial), MISSING_RATE_PLACEHOLDER);
  assert.equal(diffTotalText({ total_reval_diff: null }), MISSING_RATE_PLACEHOLDER);
});

test('缺牌价提示点名币种与期末日', () => {
  assert.equal(missingRateText({ missing_rates: [], period_end: '2060-01-31' }), null);
  const text = missingRateText({ missing_rates: ['EUR', 'HKD'], period_end: '2060-01-31' });
  assert.ok(text);
  assert.ok(text.includes('EUR'));
  assert.ok(text.includes('HKD'));
  assert.ok(text.includes('2060-01-31'));
  assert.ok(text.includes('汇率 1'), '必须说明没有按 1 折算：' + text);
});

test('账面口径标签：多币种科目要在页面上说清「这不是整科目的账面」', () => {
  assert.equal(basisLabel('ACCOUNT'), '科目合计');
  assert.equal(basisLabel('CURRENCY'), '该币种账面');
  assert.ok(basisHint('CURRENCY').includes('多个外币币种'));
  assert.ok(basisHint('ACCOUNT').includes('历次调汇'));
});

test('币种显示名 / 期间状态 / 空表提示', () => {
  assert.equal(currencyLabel('USD', '美元'), 'USD 美元');
  assert.equal(currencyLabel('USD', null), 'USD');
  assert.equal(periodStatusText('OPEN'), '未结账');
  assert.equal(periodStatusText('CLOSED'), '已结账');
  assert.equal(periodStatusText(null), '未建账·按自然月推算');

  assert.ok(emptyText({ currency_filter: null }, 'CNY').includes('没有外币核算科目'));
  assert.ok(emptyText({ currency_filter: 'CNY' }, 'CNY').includes('本位币'));
  const usdEmpty = emptyText({ currency_filter: 'USD' }, 'CNY');
  assert.ok(usdEmpty.includes('USD'), '筛了币种要说出来：' + usdEmpty);
});
