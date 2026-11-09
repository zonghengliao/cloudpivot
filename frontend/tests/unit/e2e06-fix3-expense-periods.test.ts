/**
 * E2E06-FIX-3 ② 期间口径明示的纯逻辑单测。
 *
 * 背景：同一张报销单上有**两个期间** —— 费用归属期间（预算占用按它归集）与记账期间
 * （凭证按审批通过日所属期间记账，ARCH-6 §4.5.4）。跨期报销时两者不同是正常的，
 * 界面必须两个都显示并给提示，否则用户会以为「凭证在 11 月、占用在 2 月」哪里算错了。
 *
 * 这里钉住的是「什么时候该提示」这条口径 —— `vue-tsc` 抓不到，页面渲染也看不出来：
 * · 未入账（`accounting_period_code = null`）→ 不提示（否则每张草稿都红一条）；
 * · 已入账且同年同月 → 不提示；
 * · 已入账且跨期 → 提示。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { accountingPeriodText, isPeriodMismatch } from '@/views/business/erp/expense/expense-logic';

test('未入账：只有费用期间，没有记账期间 → 不算跨期、显示「未记账」', () => {
  const report = { expense_period_code: '2027-02', accounting_period_code: null };
  assert.equal(isPeriodMismatch(report), false);
  assert.equal(accountingPeriodText(report), '未记账');
});

test('已入账且同年同月：两个期间相同 → 不提示', () => {
  const report = { expense_period_code: '2026-11', accounting_period_code: '2026-11' };
  assert.equal(isPeriodMismatch(report), false);
  assert.equal(accountingPeriodText(report), '2026-11');
});

test('跨期报销：2027-02 的费用在 2026-11 审批 → 提示，且两个值都能显示', () => {
  const report = { expense_period_code: '2027-02', accounting_period_code: '2026-11' };
  assert.equal(isPeriodMismatch(report), true);
  assert.equal(accountingPeriodText(report), '2026-11');
  assert.equal(report.expense_period_code, '2027-02');
});

test('反向跨期（费用在 2026-12、次年 1 月审批）同样提示', () => {
  const report = { expense_period_code: '2026-12', accounting_period_code: '2027-01' };
  assert.equal(isPeriodMismatch(report), true);
});
