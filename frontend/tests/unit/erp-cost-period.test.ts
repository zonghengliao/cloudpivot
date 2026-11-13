/**
 * 成本核算页纯逻辑单测（COST-B3-11 / ARCH-9 §6.2.2 五步向导）。
 *
 * 重点钉五件事（都是「错了会让用户点出 400 / 看到错数字」的地方）：
 *
 * ① **状态 → 步骤映射**：从列表点进 `DRAFT` 不该回到第 ① 步，`SETTLED` 不该停在提交审批；
 * ② **按钮门禁**：`SUBMITTED` 不能结算、`REVERSED` 一切都关掉、非 `DRAFT` 不能重算；
 * ③ **高危关键字**：`结算` / `红冲` 必须逐字全等（`结算。` 不算）；
 * ④ **红冲原因**：≥10 个**字符**（中文一字算一，不是字节）；
 * ⑤ **合计口径**：`summaryOf` 按计算单现算（单头 `total_*` 在 `DRAFT` 阶段恒 0，不能拿它显示）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  COST_PERIOD_PERMISSIONS,
  COST_PERIOD_STATUSES,
  LABOR_SOURCE_LABELS,
  REVERSE_KEYWORD,
  REVERSE_REASON_MIN,
  SETTLE_KEYWORD,
  type CostSheetItem,
} from '@/api/business/erp/cost-period-kinds';
import {
  LAST_STEP,
  WIZARD_STEPS,
  canCancel,
  canEdit,
  canNextFrom,
  canRecalc,
  canReverse,
  canSettle,
  canSubmit,
  canWithdraw,
  calcSummaryText,
  costSheetFileName,
  isFinalStatus,
  itemConserved,
  itemDetailRows,
  keywordMatches,
  laborSourceLabel,
  nextActionText,
  periodIsValid,
  qtyText,
  reasonHint,
  reasonLength,
  reversePlanText,
  reverseReasonOk,
  settleBlockedHint,
  settleKeywordOk,
  settlePlanText,
  settleSummaryText,
  statusLabel,
  statusTheme,
  stepOfStatus,
  summaryOf,
  unitText,
  voucherEntries,
  wizardTitle,
} from '@/views/business/erp/cost-period/cost-period-logic';

/** 一张「算例 A」口径的计算单行（§4.7.5：料 19000 / 工 4000 / 费 4000 / 完工 22311.1104） */
function sampleItem(overrides: Partial<CostSheetItem> = {}): CostSheetItem {
  return {
    id: 'item-1',
    cost_object_id: 'obj-1',
    object_no: 'CB-202610-0001',
    product_name: '成品 A',
    wip_beg_qty: '0.0000',
    wip_beg_cost: '0.0000',
    completed_qty: '800.0000',
    wip_end_qty: '200.0000',
    completion_pct: '50.00',
    equiv_material: '1000.0000',
    equiv_conversion: '900.0000',
    mat_cur: '19000.0000',
    labor_cur: '4000.0000',
    oh_cur: '4000.0000',
    unit_material: '19.000000',
    unit_labor: '4.444444',
    unit_overhead: '4.444444',
    finished_cost: '22311.1104',
    wip_end_cost: '4688.8896',
    unit_finished_cost: '27.888888',
    stock_in_id: null,
    status: 'CALC',
    ...overrides,
  };
}

// ==================== ① 词表与状态 ====================

test('状态值域 / 中文名 / 配色与库层 CHECK 逐值对齐', () => {
  assert.deepEqual(
    [...COST_PERIOD_STATUSES],
    ['DRAFT', 'SUBMITTED', 'APPROVED', 'SETTLED', 'CLOSED', 'CANCELLED', 'REVERSED'],
  );
  // 设计草稿期的 PENDING 不是库里的值：页面绝不能把它当状态用
  assert.equal(COST_PERIOD_STATUSES.includes('PENDING' as never), false);
  assert.equal(statusLabel('SUBMITTED'), '已提交待审');
  assert.equal(statusLabel('REVERSED'), '已红冲');
  assert.equal(statusLabel(null), '—');
  assert.equal(statusLabel('WEIRD'), 'WEIRD', '未知状态原样显示，不留空白');
  assert.equal(statusTheme('SETTLED'), 'success');
  assert.equal(statusTheme('REVERSED'), 'danger');
  assert.equal(statusTheme('UNKNOWN'), 'default');
  assert.equal(laborSourceLabel('MANUAL'), '手工台账');
  assert.equal(laborSourceLabel('PAYROLL'), LABOR_SOURCE_LABELS.PAYROLL);
});

test('权限码 5 个，形状 business:erp:cost-period:<动作>', () => {
  const codes = Object.values(COST_PERIOD_PERMISSIONS);
  assert.equal(codes.length, 5);
  for (const code of codes) {
    assert.match(code, /^business:erp:cost-period:[a-z]+$/);
  }
  assert.equal(COST_PERIOD_PERMISSIONS.reverse, 'business:erp:cost-period:reverse');
});

// ==================== ② 五步映射与门禁 ====================

test('五步标题与设计 §6.2.2 一致（5 步、末步下标 4）', () => {
  assert.equal(WIZARD_STEPS.length, 5);
  assert.equal(LAST_STEP, 4);
  assert.deepEqual(
    WIZARD_STEPS.map((item) => item.title),
    ['选期间', '料工费确认', '试算', '提交审批', '结算'],
  );
});

test('状态 → 停在第几步（用户从列表点进来不该被要求重走一遍）', () => {
  assert.equal(stepOfStatus(null), 0, '新开批次从选期间开始');
  assert.equal(stepOfStatus('DRAFT'), 2, '草稿回来是「试算 / 改数」');
  assert.equal(stepOfStatus('SUBMITTED'), 3, '审批中回来是看进度');
  assert.equal(stepOfStatus('APPROVED'), 4, '批完就能结算');
  assert.equal(stepOfStatus('SETTLED'), 4, '已结算也是第 ⑤ 步（看结果 / 红冲）');
  assert.equal(stepOfStatus('CLOSED'), 4);
  assert.equal(stepOfStatus('CANCELLED'), 0, '作废后重开');
  assert.equal(stepOfStatus('REVERSED'), 0, '红冲后重开');
});

test('按钮门禁：每个状态能做什么只在一处定义', () => {
  assert.equal(canSubmit('DRAFT'), true);
  assert.equal(canSubmit('SUBMITTED'), false);
  assert.equal(canWithdraw('SUBMITTED'), true);
  assert.equal(canWithdraw('DRAFT'), false);
  assert.equal(canSettle('APPROVED'), true);
  assert.equal(canSettle('SUBMITTED'), false, '审批中不能结算');
  assert.equal(canSettle('SETTLED'), false);
  assert.equal(canReverse('SETTLED'), true);
  assert.equal(canReverse('APPROVED'), false);
  assert.equal(canRecalc('DRAFT'), true);
  assert.equal(canRecalc('SUBMITTED'), false, '提交后数据冻结');
  assert.equal(canCancel('DRAFT'), true);
  assert.equal(canCancel('APPROVED'), false);
  assert.equal(canEdit('DRAFT'), true);
  assert.equal(canEdit('SETTLED'), false);
  for (const status of ['CANCELLED', 'REVERSED'] as const) {
    assert.equal(isFinalStatus(status), true);
    assert.equal(canSubmit(status) || canSettle(status) || canReverse(status) || canCancel(status), false);
  }
});

test('结算前置未满足要说清「为什么」而不是把按钮藏掉', () => {
  assert.match(settleBlockedHint('APPROVED'), /可以结算/);
  assert.match(settleBlockedHint('SUBMITTED'), /需审批通过后才能结算/);
  assert.match(settleBlockedHint('DRAFT'), /先提交审批/);
  assert.match(settleBlockedHint('CANCELLED'), /已作废/);
  assert.match(settleBlockedHint('SETTLED'), /红冲/);
  // 联合类型外的状态值只在运行期出现（后端加了状态、前端还没跟上）—— TS 层造不出来。
  // 这里用 `null` 覆盖同一条兜底分支：函数末尾的「当前状态不能结算」。
  assert.equal(settleBlockedHint(null), '当前状态不能结算');
});

test('「下一步」放行：① 要期间合法且未结账；② 永不阻断；③ 要批次', () => {
  const base = { period: '2026-10', periodClosed: false, hasBatch: false };
  assert.equal(canNextFrom(0, base), true);
  assert.equal(canNextFrom(0, { ...base, period: '' }), false);
  assert.equal(canNextFrom(0, { ...base, period: '2026-13' }), false);
  assert.equal(canNextFrom(0, { ...base, periodClosed: true }), false, '已结账要反结账');
  assert.equal(canNextFrom(1, base), true, '缺口不阻断（设计 §6.2.2 ②）');
  assert.equal(canNextFrom(2, base), false, '没批次不能去提交审批');
  assert.equal(canNextFrom(2, { ...base, hasBatch: true }), true);
  assert.equal(canNextFrom(3, base), true);
});

test('期间格式：只认 yyyy-MM（与库层 CHECK 同款）', () => {
  assert.equal(periodIsValid('2026-10'), true);
  assert.equal(periodIsValid(' 2026-01 '), true);
  assert.equal(periodIsValid('2026-13'), false);
  assert.equal(periodIsValid('2026-00'), false);
  assert.equal(periodIsValid('2026-1'), false);
  assert.equal(periodIsValid(''), false);
  assert.equal(periodIsValid(null), false);
});

// ==================== ③ 高危关键字 / 原因 ====================

test('结算关键字必须逐字全等（标点 / 空格都不算）', () => {
  assert.equal(SETTLE_KEYWORD, '结算');
  assert.equal(settleKeywordOk('结算'), true);
  assert.equal(settleKeywordOk('  结算 '), true, '首尾空白容忍');
  assert.equal(settleKeywordOk('结算。'), false);
  assert.equal(settleKeywordOk('結算'), false, '繁体不算');
  assert.equal(settleKeywordOk(''), false);
  assert.equal(keywordMatches('红冲', REVERSE_KEYWORD), true);
  assert.equal(keywordMatches('红冲吧', REVERSE_KEYWORD), false);
});

test('红冲原因按「字符」算 ≥10（中文一字算一）', () => {
  assert.equal(REVERSE_REASON_MIN, 10);
  assert.equal(reasonLength('完工数量录错要重算'), 9);
  assert.equal(reasonLength('完工数量录错了要重算'), 10);
  assert.equal(reverseReasonOk('完工数量录错了要重算'), true);
  assert.equal(reverseReasonOk('错的'), false);
  assert.equal(reverseReasonOk(''), false);
  assert.match(reasonHint('') ?? '', /必填/);
  assert.match(reasonHint('错的') ?? '', /至少 10 个字（现在 2 个）/);
  assert.equal(reasonHint('完工数量录错了要重算'), null);
});

// ==================== ④ 合计 / 守恒 / 明细 ====================

test('summaryOf 按计算单现算（不用单头 total_*）', () => {
  const summary = summaryOf([sampleItem()]);
  assert.equal(summary.object_count, 1);
  assert.equal(summary.total_material, '19000.0000');
  assert.equal(summary.total_labor, '4000.0000');
  assert.equal(summary.total_overhead, '4000.0000');
  assert.equal(summary.total_cost, '27000.0000');
  assert.equal(summary.total_finished, '22311.1104');
  assert.equal(summary.total_wip, '4688.8896');

  const two = summaryOf([
    sampleItem(),
    sampleItem({ mat_cur: '3000.0000', labor_cur: '0.0000', oh_cur: '0.0000', finished_cost: '3000.0000', wip_end_cost: '0.0000' }),
  ]);
  assert.equal(two.object_count, 2);
  assert.equal(two.total_cost, '30000.0000');
  assert.equal(two.total_finished, '25311.1104');

  assert.equal(summaryOf([]).total_cost, '0.0000');
});

test('单行守恒：完工转出 + 期末在制 = 期初在制 + 料 + 工 + 费', () => {
  assert.equal(itemConserved(sampleItem()), true);
  // 期末在制被手工改小 1 分 ⇒ 不守恒（库层 CHECK 会拦，页面要能一眼看出来）
  assert.equal(itemConserved(sampleItem({ wip_end_cost: '4688.8895' })), false);
});

test('明细行覆盖 4 个数量口径 / 6 个金额 / 4 个单价（可展开看明细）', () => {
  const rows = itemDetailRows(sampleItem());
  assert.equal(rows.filter((row) => row.group === '数量').length, 6);
  assert.equal(rows.filter((row) => row.group === '金额').length, 6);
  assert.equal(rows.filter((row) => row.group === '单位成本').length, 4);
  const unit = rows.find((row) => row.label === '完工入库单价');
  assert.equal(unit?.value, '27.888888', '单价 6 位小数');
  const money = rows.find((row) => row.label === '完工转出成本');
  assert.match(money?.value ?? '', /22,311\.11/, '金额展示改三位一组');
  const pct = rows.find((row) => row.label === '期末在制完工进度');
  assert.equal(pct?.value, '50%');
});

test('数量 / 单价格式化去尾零但不丢精度', () => {
  assert.equal(qtyText('2805.0000'), '2805');
  assert.equal(qtyText('0.5000'), '0.5');
  assert.equal(qtyText(null), '0');
  assert.equal(unitText('19.000000'), '19.000000');
  assert.equal(unitText('4.4444'), '4.444400');
});

test('试算摘要：有行给合计、无行明确说「还没有计算单」', () => {
  const detail = {
    items: [sampleItem()],
  } as unknown as Parameters<typeof calcSummaryText>[0];
  const text = calcSummaryText(detail);
  assert.match(text, /1 个成本对象/);
  assert.match(text, /27,000\.00/);
  assert.match(calcSummaryText({ items: [] } as unknown as typeof detail), /还没有成本计算单/);
  assert.equal(calcSummaryText(null), '还没有试算结果');
});

// ==================== ⑤ 结算 / 红冲 / 凭证 ====================

test('结算计划文案：N 张入库单 + 凭证张数按金额是否为零收敛', () => {
  const full = settlePlanText({ items: [sampleItem()] });
  assert.match(full, /将生成 1 张生产入库单 \+ 3 张凭证（人工计提 \/ 制造费用分摊 \/ 完工结转）/);
  assert.match(full, /投入合计 27,000\.00/);
  assert.match(full, /完工转出 22,311\.11/);

  // 人工为 0 时不承诺「人工计提凭证」（后端结算时同样按金额判断是否出这张）
  const noLabor = settlePlanText({
    items: [sampleItem({ labor_cur: '0.0000', mat_cur: '2000.0000', oh_cur: '1000.0000', finished_cost: '3000.0000', wip_end_cost: '0.0000' })],
  });
  assert.match(noLabor, /2 张凭证（制造费用分摊 \/ 完工结转）/);
  assert.match(settlePlanText(null), /0 张生产入库单/);
});

test('步⑤ 顶部文案：未结算「将生成」，已结算/已红冲说「已…」（不许用未来时）', () => {
  const approved = {
    status: 'APPROVED',
    items: [sampleItem()],
    object_count: 1,
    total_finished: '0.0000',
  } as unknown as Parameters<typeof settleSummaryText>[0];
  assert.match(settleSummaryText(approved), /将生成 1 张生产入库单/);

  const settled = {
    status: 'SETTLED',
    items: [],
    object_count: 2,
    total_finished: '23561.1100',
  } as unknown as Parameters<typeof settleSummaryText>[0];
  const settledText = settleSummaryText(settled);
  assert.match(settledText, /^已结算：2 张生产入库单 \+ 3 张凭证/);
  assert.match(settledText, /23,561\.11/);
  assert.equal(settleSummaryText(null), '');

  const reversed = {
    status: 'REVERSED',
    items: [],
    object_count: 2,
    total_finished: '23561.1100',
  } as unknown as Parameters<typeof settleSummaryText>[0];
  assert.match(settleSummaryText(reversed), /^已红冲/);
});

test('红冲计划文案点名批次号、对象数与金额', () => {
  const detail = {
    batch_no: 'CS-202610-0001',
    object_count: 2,
    total_finished: '22311.1104',
  } as unknown as Parameters<typeof reversePlanText>[0];
  const text = reversePlanText(detail);
  assert.match(text, /CS-202610-0001/);
  assert.match(text, /2 个成本对象的库存/);
  assert.match(text, /22,311\.11/);
  assert.match(text, /批次置「已红冲」/);
  assert.equal(reversePlanText(null), '红冲将反向撤销这次结算');
});

test('三张凭证按「人工 → 费用 → 完工」顺序，空 id 不显示', () => {
  const vouchers = voucherEntries({
    labor_voucher_id: 'v-labor',
    overhead_voucher_id: null,
    finish_voucher_id: 'v-finish',
  });
  assert.deepEqual(vouchers, [
    { label: '人工计提凭证', id: 'v-labor' },
    { label: '完工结转凭证', id: 'v-finish' },
  ]);
  assert.deepEqual(voucherEntries(null), []);
});

test('向导标题 / 列表「该做什么」文案', () => {
  assert.match(wizardTitle('2026-10', 'CS-202610-0001'), /CS-202610-0001（期间 2026-10）/);
  assert.match(wizardTitle('', null), /新建批次/);
  assert.match(nextActionText('DRAFT'), /继续核算/);
  assert.match(nextActionText('SUBMITTED'), /可撤回/);
  assert.match(nextActionText('APPROVED'), /可以结算/);
  assert.match(nextActionText('SETTLED'), /可红冲/);
  assert.match(nextActionText('REVERSED'), /可重开/);
  assert.equal(nextActionText(null), '—');
});

test('导出文件名与后端 `file_name` 同形（成本计算单_<批次号>.xlsx）', () => {
  assert.equal(costSheetFileName('CS-202610-0001'), '成本计算单_CS-202610-0001.xlsx');
  // 空 / null 不生成「成本计算单_.xlsx」这种既像空文件又没法检索的名字
  assert.equal(costSheetFileName(''), '成本计算单_未命名批次.xlsx');
  assert.equal(costSheetFileName(null), '成本计算单_未命名批次.xlsx');
  assert.equal(costSheetFileName('  CS-1  '), '成本计算单_CS-1.xlsx');
});
