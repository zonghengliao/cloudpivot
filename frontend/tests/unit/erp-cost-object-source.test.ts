/**
 * 成本对象「料从哪来 / 货去哪了」反查的纯逻辑单测（COST-B4-2 / ARCH-9 §8.1）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 本卡在 `.vue` 里新增了「关联领料单 + 完工入库单 + 5 条对平判据」三块，判据口径
 * 全部在后端（`cost_object_dto::build_source_checks`）现算；前端只负责**译名 + 展示 + 跳转**。
 * 这三件事都是纯逻辑，写成函数才能钉住：
 *
 * | 函数 | 钉住什么 | 漂移的后果 |
 * |---|---|---|
 * | `sourceStatusLabel` | 出库/入库两套状态词的中文 | 界面出现英文 `PENDING_POST` |
 * | `sourceDocCollectedText` | 「这笔料进没进成本」的一句话 | 用户看不出哪张单没归集 |
 * | `sourceDocCollectedTheme` | 状态色（绿 / 黄 / 灰 / 红） | 未归集与已归集同色 ⇒ 漏看 |
 * | `sourceDocRoute` | 点单号跳到**哪个页面、带哪个参数** | 假链接 / 404 |
 * | `sourceChecksSummary` | 「几条对不上」的汇总文案 | 红了却不说是哪条 |
 * | `sourcesHeadline` | 标题旁的一句总述（取后端合计） | 前端重算 ⇒ 与表格合计对不上 |
 *
 * 跳转口径来自 `shared/source-route.ts`（VTRACE 唯一真相表）：
 * `STOCK_OUT` → `/business/erp/stock-out?out_no=`（按单号筛）、
 * `STOCK_IN` → `/business/erp/stock-in?in_id=`（直开详情）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type {
  CostObjectSourceCheck,
  CostObjectSourceDoc,
  CostObjectSources,
} from '@/api/business/erp/cost-object-kinds';
import {
  sourceChecksSummary,
  sourceDocCollectedText,
  sourceDocCollectedTheme,
  sourceDocRoute,
  sourceDocTypeLabel,
  sourcesHeadline,
  sourceStatusLabel,
  sourceTotalsCountText,
} from '@/views/business/erp/cost-object/cost-object-logic';

/** 造一条领料单反查行（默认：已过账 + 已归集）。 */
function issueDoc(over: Partial<CostObjectSourceDoc> = {}): CostObjectSourceDoc {
  return {
    doc_type: 'STOCK_OUT',
    id: '11111111-1111-1111-1111-111111111111',
    doc_no: 'OUT-20261105-000001',
    doc_date: '2026-11-05',
    status: 'POSTED',
    warehouse_name: '原料仓',
    line_count: 2,
    qty: '10',
    amount: '100',
    voucher_id: '22222222-2222-2222-2222-222222222222',
    voucher_no: 'PZ-202611-0001',
    period_codes: ['2026-11'],
    batch_no: null,
    collected: true,
    collected_qty: '10',
    collected_amount: '100',
    remark: '生产领用',
    ...over,
  };
}

/** 造一条完工入库单反查行（默认：有效 POSTED）。 */
function receiptDoc(over: Partial<CostObjectSourceDoc> = {}): CostObjectSourceDoc {
  return {
    doc_type: 'STOCK_IN',
    id: '33333333-3333-3333-3333-333333333333',
    doc_no: 'IN-20261130-000001',
    doc_date: '2026-11-30',
    status: 'POSTED',
    warehouse_name: '成品仓',
    line_count: 1,
    qty: '80',
    amount: '2000',
    voucher_id: '44444444-4444-4444-4444-444444444444',
    voucher_no: 'PZ-202611-0005',
    period_codes: ['2026-11'],
    batch_no: 'CS-202611-0001',
    collected: true,
    collected_qty: '80',
    collected_amount: '2000',
    remark: null,
    ...over,
  };
}

function check(over: Partial<CostObjectSourceCheck> = {}): CostObjectSourceCheck {
  return {
    key: 'issue_collected_amount',
    label: '领料金额 = 归集直接材料',
    left_label: '已过账领料明细金额',
    left: '100',
    right_label: '归集料行金额',
    right: '100',
    diff: '0',
    ok: true,
    hint: '去成本核算重算归集',
    ...over,
  };
}

test('状态 / 类型译名逐字对齐后端词表', () => {
  assert.equal(sourceStatusLabel('POSTED'), '已过账');
  assert.equal(sourceStatusLabel('DRAFT'), '草稿');
  assert.equal(sourceStatusLabel('PENDING_POST'), '待过账');
  assert.equal(sourceStatusLabel('CANCELLED'), '已作废');
  // 不认识的值原样返回（后端加新状态时页面至少能看出那个英文词，不编「未知」）
  assert.equal(sourceStatusLabel('SOMETHING_NEW'), 'SOMETHING_NEW');
  assert.equal(sourceStatusLabel(null), '—');
  assert.equal(sourceDocTypeLabel('STOCK_OUT'), '领料出库单');
  assert.equal(sourceDocTypeLabel('STOCK_IN'), '完工入库单');
});

test('领料行的归集情况四种文案（已归集 / 未归集 / 未过账）', () => {
  assert.equal(sourceDocCollectedText(issueDoc()), '已归集 · 2026-11');
  // 跨期补录：两个期间都列出来
  assert.equal(
    sourceDocCollectedText(issueDoc({ period_codes: ['2026-09', '2026-10'] })),
    '已归集 · 2026-09 / 2026-10',
  );
  // 部分归集：差额摆出来（已归集 6 / 10）
  assert.equal(
    sourceDocCollectedText(issueDoc({ collected: false, collected_qty: '6', collected_amount: '60' })),
    '未归集（已归集 6 / 10）',
  );
  // 未过账：不计入成本（合计数里也没有它）
  assert.equal(
    sourceDocCollectedText(issueDoc({ status: 'DRAFT', collected: false, collected_qty: '0' })),
    '草稿，不计入成本',
  );
});

test('入库行只说「已入账 / 已作废」（它没有归集概念）', () => {
  assert.equal(sourceDocCollectedText(receiptDoc()), '已入账');
  assert.equal(sourceDocCollectedText(receiptDoc({ status: 'CANCELLED', collected: false })), '已作废');
  assert.equal(sourceDocCollectedTheme(receiptDoc()), 'success');
  assert.equal(sourceDocCollectedTheme(receiptDoc({ status: 'CANCELLED', collected: false })), 'danger');
});

test('状态色：已归集绿、未归集黄、未过账灰', () => {
  assert.equal(sourceDocCollectedTheme(issueDoc()), 'success');
  assert.equal(sourceDocCollectedTheme(issueDoc({ collected: false })), 'warning');
  assert.equal(sourceDocCollectedTheme(issueDoc({ status: 'DRAFT', collected: false })), 'default');
});

test('点行跳原单据：出库按单号筛、入库按 id 直开', () => {
  const issueRoute = sourceDocRoute(issueDoc());
  assert.equal(issueRoute?.path, '/business/erp/stock-out');
  assert.deepEqual(issueRoute?.query, { out_no: 'OUT-20261105-000001' });

  const receiptRoute = sourceDocRoute(receiptDoc());
  assert.equal(receiptRoute?.path, '/business/erp/stock-in');
  assert.deepEqual(receiptRoute?.query, { in_id: '33333333-3333-3333-3333-333333333333' });
});

test('跳不了就不给链接（宁可不跳，不给假链接）', () => {
  // 单号为空：STOCK_OUT 走 by='no'，没单号解析不出参数
  assert.equal(sourceDocRoute(issueDoc({ doc_no: '' })), null);
  // 入库走 by='id'，id 为空同理
  assert.equal(sourceDocRoute(receiptDoc({ id: '' })), null);
});

test('判据汇总：全绿 / 点名不平的那几条', () => {
  const green = sourceChecksSummary([check(), check({ key: 'b', label: '完工资' })]);
  assert.equal(green.total, 2);
  assert.equal(green.failed, 0);
  assert.equal(green.allOk, true);
  assert.equal(green.text, '2 条判据全部对得上');

  const red = sourceChecksSummary([
    check(),
    check({ key: 'b', label: '完工入库数量 = 计算单累计完工', ok: false, diff: '-8' }),
    check({ key: 'c', label: '已过账领料都已归集', ok: false }),
  ]);
  assert.equal(red.failed, 2);
  assert.equal(red.allOk, false);
  assert.equal(red.text, '2 条对不上：完工入库数量 = 计算单累计完工、已过账领料都已归集');

  // 空判据：不声称「全绿」（没算过就不算绿）
  assert.equal(sourceChecksSummary([]).allOk, false);
});

test('合计行数文案 + 标题总述都用后端合计（前端不重算）', () => {
  assert.equal(sourceTotalsCountText({ doc_count: 3, line_count: 5, qty: '10', amount: '100' }), '3 张 · 5 行');

  const sources: CostObjectSources = {
    object_id: '55555555-5555-5555-5555-555555555555',
    object_no: 'CB-20261105-000001',
    product_name: 'MAT-0001 钢轴',
    status: 'OPEN',
    completed_qty: '80',
    wip_qty: '20',
    issues: [issueDoc()],
    issue_totals: { doc_count: 1, line_count: 2, qty: '10', amount: '100' },
    issue_posted_totals: { doc_count: 1, line_count: 2, qty: '10', amount: '100' },
    receipts: [receiptDoc()],
    receipt_totals: { doc_count: 1, line_count: 1, qty: '80', amount: '2000' },
    receipt_cancelled_count: 0,
    checks: [check()],
    all_ok: true,
  };
  assert.equal(
    sourcesHeadline(sources),
    '领料 1 张 10 / 100.00 · 完工入库 1 张 80 / 2,000.00',
    '一千分位与两位小数来自 shared/money 的 formatMoney',
  );
});
