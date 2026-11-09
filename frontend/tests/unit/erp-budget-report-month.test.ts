/**
 * 预算执行报表「年度 vs 月度」双口径的纯逻辑单测（E2E06-FIX-2）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么单独钉：
 * 1. 报表主列必须**月度优先** —— 月度闸才是提交侧实际控制的闸门（超了就 409 / 标超支），
 *    「年度没超、本月超了」这种行如果不先报月度，财务就会以为没事（这正是 E2E-06 发现的问题）；
 * 2. 汇总卡片的两个计数（`overCount` 年度 / `monthOverCount` 月度）不能混在一起数；
 * 3. 后端 `ReportRes` 的字段名是**逐字**抄的（`month_over` / `month_over_amount` /
 *    `month_available` / `month_period`），前端只做展示组合，不重算任何金额口径。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { BudgetReportRow } from '@/api/business/erp/budget';
import { reportBadge, summarizeReport } from '@/views/business/erp/budget/budget-logic';

/** 造一行报表数据（默认：FY2027 通讯费槽，年度没超、月度也没超） */
function reportRow(overrides: Partial<BudgetReportRow> = {}): BudgetReportRow {
  return {
    budget_id: 'b0000000-0000-0000-0000-000000000001',
    fiscal_year: 2027,
    dept_id: 'd0000000-0000-0000-0000-000000000001',
    dept_name: 'E2E06 预算演练部',
    account_id: 'a0000000-0000-0000-0000-000000000001',
    account_code: '5602.05',
    account_name: '管理费用-通讯费',
    total_amount: '12000.0000',
    occupied: '0.0000',
    consumed: '1500.0000',
    used: '1500.0000',
    available: '10500.0000',
    exec_ratio: '12.50',
    over: false,
    effective_from: '2026-12-01',
    effective: true,
    month_period: '2027-02',
    month_limit: '1000.0000',
    month_used: '1500.0000',
    month_available: '-500.0000',
    month_over: true,
    month_over_amount: '500.0000',
    months: [],
    ...overrides,
  };
}

test('年度没超但本月超 → 主列报「本月超 500.00」（月度优先）', () => {
  const row = reportRow();
  const badge = reportBadge(row);
  assert.equal(badge.text, '本月超 500.00');
  assert.equal(badge.theme, 'danger');
  assert.match(badge.hint, /2027-02/);
});

test('年度和月度都超 → 仍只显示月度那条（同一行不给两个红标）', () => {
  const row = reportRow({
    total_amount: '1000.0000',
    consumed: '1500.0000',
    used: '1500.0000',
    available: '-500.0000',
    over: true,
  });
  const badge = reportBadge(row);
  assert.equal(badge.text, '本月超 500.00');
  assert.equal(badge.theme, 'danger');
});

test('只有年度超 → 报「年度超」', () => {
  const row = reportRow({
    available: '-2000.0000',
    over: true,
    month_over: false,
    month_over_amount: null,
    month_available: '200.0000',
    month_used: '800.0000',
  });
  const badge = reportBadge(row);
  assert.equal(badge.text, '年度超');
  assert.equal(badge.theme, 'danger');
});

test('未生效（effective=false）→ 黄色「未生效」，且提示说的是年度期末口径', () => {
  const row = reportRow({
    effective_from: '2028-06-01',
    effective: false,
    month_over: false,
    month_over_amount: null,
    month_available: '1000.0000',
    month_used: '0.0000',
  });
  const badge = reportBadge(row);
  assert.equal(badge.text, '未生效');
  assert.equal(badge.theme, 'warning');
  assert.match(badge.hint, /2028-06-01/);
  assert.match(badge.hint, /2027 年度期末/);
});

test('都在额度内 → 中性「正常」', () => {
  const row = reportRow({
    month_over: false,
    month_over_amount: null,
    month_available: '1000.0000',
    month_used: '0.0000',
  });
  const badge = reportBadge(row);
  assert.equal(badge.text, '正常');
  assert.equal(badge.theme, 'default');
});

test('没有月度分解（month_period=null）→ 不报超支，主列「正常」', () => {
  const row = reportRow({
    month_period: null,
    month_limit: null,
    month_used: null,
    month_available: null,
    month_over: false,
    month_over_amount: null,
  });
  assert.equal(reportBadge(row).text, '正常');
});

test('汇总卡片：年度超支与本月超支分开数（同一个月超的槽两边都算）', () => {
  const rows = [
    reportRow(), // 本月超、年度没超
    reportRow({ budget_id: 'b2', over: true, available: '-500.0000' }), // 两个都超
    reportRow({
      budget_id: 'b3',
      month_over: false,
      month_over_amount: null,
      month_available: '1000.0000',
      month_used: '0.0000',
    }), // 都没超
  ];
  const summary = summarizeReport(rows);
  assert.equal(summary.count, 3);
  assert.equal(summary.overCount, 1, '年度超支只有 1 个槽');
  assert.equal(summary.monthOverCount, 2, '本月超支有 2 个槽（含年度也超的那个）');
  assert.equal(summary.usedCents, 1500 * 100 * 3);
});
