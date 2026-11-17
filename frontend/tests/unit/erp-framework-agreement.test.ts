/**
 * 框架协议页面纯逻辑单测（M4-11：协议台账 + 执行量）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 为什么前端也要把这几条钉一遍
 *
 * 后端 `framework_agreement_execution_dto.rs` 已经把「剩余可为负 / null 是不限量 / 状态五态」
 * 算对了；前端这一份钉的是**同一个口径在渲染层不会被打回 0**——历史上最容易翻车的三处：
 *
 * | 组 | 钉住什么 | 翻车后果 |
 * |----|----------|----------|
 * | ① | `null` 封顶 / 剩余 / 执行率 → 「不限」，**不是** 0 | 界面显示「剩余 0」，用户以为没额度了 |
 * | ② | 负数剩余 → 「超 X」，**不截断** | 超上限的协议在报表上看不出来 |
 * | ③ | 金额按字符串舍 2 位（不经浮点） | 亿元级金额尾数被双精度吃掉 |
 * | ④ | 分页入参：空串 / false 不传 | `?keyword=` 查出 0 行、`?over_only=false` 是噪音 |
 * | ⑤ | 有效期一句话按 `validity_state` 分（草稿不看日期） | 草稿被显示成「生效中」 |
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { ExecSource } from '@/api/business/erp/framework-agreement';
import {
  MAX_KEYWORD,
  PAGE_SIZE,
  UNLIMITED_TEXT,
  agreementStatusLabel,
  buildExecQuery,
  buildLedgerQuery,
  buildQuery,
  capMoneyText,
  capText,
  decimalText,
  detailTitle,
  docCountText,
  driftText,
  emptyExecFilter,
  emptyLedgerFilter,
  emptyListText,
  execStateLabel,
  execStateTheme,
  hasDrift,
  itemSummaryText,
  keywordWarning,
  lineCountsBrief,
  lineCountsText,
  materialText,
  moneyText,
  ownerText,
  pctText,
  periodText,
  priceModeLabel,
  priceTaxText,
  progressPercent,
  progressStatus,
  remainingMoneyText,
  remainingText,
  sourceKey,
  sourceOrderText,
  sourcesOf,
  sourcesSummaryText,
  sourceVerdictTheme,
  statusTheme,
  supplierText,
  taxRateText,
  timeText,
  toNumberOrNull,
  toleranceText,
  usedAmountLabel,
  usedAmountOf,
  validityLabel,
  validityLeftText,
  validityText,
  validityTheme,
} from '@/views/business/erp/framework-agreement/framework-agreement-logic';

// ==================== ① 金额 / 数值展示 ====================

test('moneyText 按字符串舍到 2 位（不经过浮点）', () => {
  assert.equal(moneyText('300.0000'), '300.00');
  assert.equal(moneyText('1234.5678'), '1234.57');
  assert.equal(moneyText('1.999'), '2.00', '进位要跨整数位');
  assert.equal(moneyText('9.999'), '10.00');
  assert.equal(moneyText('0'), '0.00');
  assert.equal(moneyText('0.5'), '0.50');
  assert.equal(moneyText('.5'), '0.50', '后端不会给这种形态，但别崩');
  assert.equal(moneyText(-12.3456), '-12.35', '负数保留符号（剩余金额可为负）');
  assert.equal(moneyText('12345678901234.5678'), '12345678901234.57', '亿元级以上不掉尾数');
  assert.equal(moneyText('99999999999999999999.995'), '100000000000000000000.00', '纯字符串进位不会溢出');
});

test('moneyText 空值给占位、非数字原样返回', () => {
  assert.equal(moneyText(null), '—');
  assert.equal(moneyText(undefined), '—');
  assert.equal(moneyText(''), '—');
  assert.equal(moneyText('abc'), 'abc', '出参形状变了也不要把 NaN 打到界面上');
});

test('decimalText 原样展示（审计要对得上库里那几位）', () => {
  assert.equal(decimalText('100.000000'), '100.000000');
  assert.equal(decimalText('0'), '0');
  assert.equal(decimalText(null), '—');
});

test('toNumberOrNull：空串 / null 是 null，0 不是', () => {
  assert.equal(toNumberOrNull('0'), 0);
  assert.equal(toNumberOrNull(0), 0);
  assert.equal(toNumberOrNull(''), null);
  assert.equal(toNumberOrNull(null), null);
  assert.equal(toNumberOrNull(undefined), null);
  assert.equal(toNumberOrNull('not-a-number'), null);
});

// ==================== ② 不限量 / 负数剩余（这一页的核心口径） ====================

test('封顶 null = 不限量，绝不能显示成 0', () => {
  assert.equal(capText(null), UNLIMITED_TEXT);
  assert.equal(capText(undefined), UNLIMITED_TEXT);
  assert.equal(capText('100.000000'), '100.000000');
  assert.equal(capText('0'), '0', '0 是 0（库层挡了 max_qty > 0，但显示层不许篡改）');
  assert.equal(capMoneyText(null), UNLIMITED_TEXT);
  assert.equal(capMoneyText('1000.5000'), '1000.50');
});

test('剩余：null → 不限、负数 → 「超 X」、正数原样', () => {
  assert.equal(remainingText(null), UNLIMITED_TEXT);
  assert.equal(remainingText('70.000000'), '70.000000');
  assert.equal(remainingText('-20.000000'), '超 20.000000', '超上限必须显形，不许截成 0');
  assert.equal(remainingText('-0.0001'), '超 0.0001', '哪怕超一点点也要看得见');
  assert.equal(remainingText('0'), '0', '恰好用满 = 剩余 0（与「不限」是两件事）');
});

test('剩余金额：同口径，多一层两位小数', () => {
  assert.equal(remainingMoneyText(null), UNLIMITED_TEXT);
  assert.equal(remainingMoneyText('-1500.5'), '超 1500.50');
  assert.equal(remainingMoneyText('999.999'), '1000.00');
});

test('执行率百分比：null → 不限（不是 0.00%），其余原样加 %', () => {
  assert.equal(pctText(null), UNLIMITED_TEXT);
  assert.equal(pctText('30.00'), '30.00%');
  assert.equal(pctText('0.00'), '0.00%', '真的 0% 与「不限」必须能分开');
  assert.equal(pctText('150.00'), '150.00%', '超上限的倍数照实显示');
});

test('进度条封顶 100（超上限靠文案与状态表达），null / 负数归 0', () => {
  assert.equal(progressPercent('30.00'), 30);
  assert.equal(progressPercent('150.00'), 100, 't-progress 画过 100 会出界');
  assert.equal(progressPercent('0.005'), 0.01);
  assert.equal(progressPercent(null), 0);
  assert.equal(progressPercent('0'), 0);
});

test('进度条状态：只有 DONE/OVER/PARTIAL 染色（不限量别染成失败）', () => {
  assert.equal(progressStatus('DONE'), 'success');
  assert.equal(progressStatus('OVER'), 'error');
  assert.equal(progressStatus('PARTIAL'), 'active');
  assert.equal(progressStatus('NOT_STARTED'), undefined);
  assert.equal(progressStatus('UNLIMITED'), undefined);
});

// ==================== ③ 状态 / 标签 ====================

test('执行状态五态：中文 + 主题全覆蓋，未知值原样', () => {
  assert.equal(execStateLabel('UNLIMITED'), '不限量');
  assert.equal(execStateLabel('NOT_STARTED'), '未开工');
  assert.equal(execStateLabel('PARTIAL'), '部分执行');
  assert.equal(execStateLabel('DONE'), '已用满');
  assert.equal(execStateLabel('OVER'), '已超限');
  assert.equal(execStateLabel('WHATEVER'), 'WHATEVER');

  assert.equal(execStateTheme('UNLIMITED'), 'primary');
  assert.equal(execStateTheme('NOT_STARTED'), 'default');
  assert.equal(execStateTheme('PARTIAL'), 'warning');
  assert.equal(execStateTheme('DONE'), 'success');
  assert.equal(execStateTheme('OVER'), 'danger');
});

test('协议状态只有三个（库里没有 EXPIRED 状态列）', () => {
  assert.equal(agreementStatusLabel('DRAFT'), '拟稿');
  assert.equal(agreementStatusLabel('SUBMITTED'), '审批中');
  assert.equal(agreementStatusLabel('APPROVED'), '生效');
  assert.equal(statusTheme('APPROVED'), 'success');
  assert.equal(statusTheme('SUBMITTED'), 'warning');
  assert.equal(statusTheme('DRAFT'), 'default');
});

test('有效期状态：过期染红、未生效不染红', () => {
  assert.equal(validityLabel('ACTIVE'), '生效中');
  assert.equal(validityLabel('EXPIRED'), '已过期');
  assert.equal(validityTheme('ACTIVE'), 'success');
  assert.equal(validityTheme('EXPIRED'), 'danger');
  assert.equal(validityTheme('NOT_STARTED'), 'default');
  assert.equal(validityTheme('SUBMITTED'), 'warning');
});

test('有效期一句话：草稿 / 审批中不看日期，其余按天数正负说人话', () => {
  assert.equal(validityText({ validity_state: 'DRAFT', days_left: 100 }), '拟稿（不生效）');
  assert.equal(validityText({ validity_state: 'SUBMITTED', days_left: 100 }), '审批中（不生效）');
  assert.equal(validityText({ validity_state: 'ACTIVE', days_left: 30 }), '生效中 · 还剩 30 天');
  assert.equal(validityText({ validity_state: 'NOT_STARTED', days_left: 40 }), '未生效 · 还有 40 天');
  assert.equal(validityText({ validity_state: 'EXPIRED', days_left: -5 }), '已过期 5 天');
  assert.equal(periodText({ start_date: '2026-01-01', end_date: '2026-12-31' }), '2026-01-01 ~ 2026-12-31');
});

test('有效期「剩余天数」短句（列表用：状态已在标签里，不重复）', () => {
  assert.equal(validityLeftText({ validity_state: 'ACTIVE', days_left: 30 }), '还剩 30 天');
  assert.equal(validityLeftText({ validity_state: 'NOT_STARTED', days_left: 40 }), '还有 40 天');
  assert.equal(validityLeftText({ validity_state: 'EXPIRED', days_left: -5 }), '已过期 5 天');
  assert.equal(validityLeftText({ validity_state: 'DRAFT', days_left: 100 }), '', '拟稿没有天数概念');
  assert.equal(validityLeftText({ validity_state: 'SUBMITTED', days_left: 100 }), '');
});

test('定价方式 / 含税口径 / 价格上浮 / 税率', () => {
  assert.equal(priceModeLabel('FIXED'), '固定价');
  assert.equal(priceModeLabel('FLOATING'), '随行就市');
  assert.equal(priceTaxText(true), '含税价');
  assert.equal(priceTaxText(false), '不含税价');

  assert.equal(toleranceText('0'), '不得高于协议价', '0 是硬拦，不是「可上浮 0%」');
  assert.equal(toleranceText('0.05'), '可上浮 5.00%');
  assert.equal(toleranceText(null), '—');

  assert.equal(taxRateText('0.13'), '13%');
  assert.equal(taxRateText('0.06'), '6%');
  assert.equal(taxRateText('0.1'), '10%');
  assert.equal(taxRateText(null), '—');
});

// ==================== ④ 汇总 / 摘要文案 ====================

test('行状态计数五段并排（不限量单独一列，不并进「其它」）', () => {
  assert.equal(
    lineCountsText({
      item_count: 5,
      unlimited_items: 1,
      not_started_items: 1,
      partial_items: 2,
      done_items: 0,
      over_items: 1,
    }),
    '共 5 行 · 不限 1 · 未开工 1 · 部分 2 · 已用满 0 · 超限 1',
  );
});

test('行状态计数短版（列表用）：只列非零桶，超限排最前', () => {
  assert.equal(
    lineCountsBrief({
      item_count: 2,
      unlimited_items: 0,
      not_started_items: 0,
      partial_items: 1,
      done_items: 0,
      over_items: 1,
    }),
    '共 2 行 · 超限 1 · 部分 1',
  );
  assert.equal(
    lineCountsBrief({
      item_count: 1,
      unlimited_items: 1,
      not_started_items: 0,
      partial_items: 0,
      done_items: 0,
      over_items: 0,
    }),
    '共 1 行 · 不限 1',
  );
  assert.equal(
    lineCountsBrief({
      item_count: 0,
      unlimited_items: 0,
      not_started_items: 0,
      partial_items: 0,
      done_items: 0,
      over_items: 0,
    }),
    '共 0 行',
    '空物料范围：只有「共 0 行」，不编出别的桶',
  );
});

test('时间戳展示：截到分钟（RFC3339 到人话），空值给占位', () => {
  assert.equal(timeText('2026-11-17T22:20:50.123456Z'), '2026-11-17 22:20');
  assert.equal(timeText('2026-11-17 22:20:50'), '2026-11-17 22:20');
  assert.equal(timeText(null), '—');
  assert.equal(timeText(''), '—');
  assert.equal(timeText('不是时间'), '不是时间', '认不出就原样给，别吞成空');
});

test('单数文案：有释放单时才带括号（解释「为什么已用量没涨」）', () => {
  assert.equal(docCountText({ order_doc_count: 2, released_doc_count: 0 }), '2 张');
  assert.equal(docCountText({ order_doc_count: 1, released_doc_count: 1 }), '1 张（释放 1）');
});

test('物料摘要 / 物料行标题', () => {
  assert.equal(itemSummaryText({ item_count: 0, unlimited_items: 0 }), '无物料范围');
  assert.equal(itemSummaryText({ item_count: 3, unlimited_items: 0 }), '3 项');
  assert.equal(itemSummaryText({ item_count: 3, unlimited_items: 1 }), '3 项（不限 1）');
  assert.equal(materialText({ material_code: 'M-A01', material_name: '钢板' }), 'M-A01 钢板');
});

test('已用金额标签按协议价口径走（含税协议看含税金额）', () => {
  assert.equal(usedAmountLabel(true), '已用金额（含税）');
  assert.equal(usedAmountLabel(false), '已用金额（不含税）');
  const head = {
    executed_amount_capped: '1130.000000',
    price_includes_tax: true,
  } as Parameters<typeof usedAmountOf>[0];
  assert.equal(usedAmountOf(head), '1130.00');
});

// ==================== ⑤ 回写分叉（booked_drift） ====================

test('回写分叉：0 不提示、非 0 给一行灰字（不弹错）', () => {
  assert.equal(driftText({ booked_qty: '30.000000', booked_drift: '0' }), '');
  assert.equal(driftText({ booked_qty: '30.000000', booked_drift: '-5.000000' }), '回写快照 30.000000（差 -5.000000）');
  assert.equal(hasDrift({ booked_drift: '0' }), false);
  assert.equal(hasDrift({ booked_drift: '0.000001' }), true);
  assert.equal(hasDrift({ booked_drift: null as unknown as string }), false, '出参缺字段时不要误报');
});

// ==================== ⑥ 下钻来源 ====================

function source(over: Partial<ExecSource>): ExecSource {
  return {
    agreement_item_id: 'item-1',
    order_id: 'order-1',
    order_no: 'PO-001',
    order_status: 'DRAFT',
    order_status_label: '草稿',
    order_date: '2026-11-18',
    order_deleted_at: null,
    line_no: 1,
    qty: '10',
    amount: '100',
    with_tax_amount: '113',
    price: '10',
    counted: true,
    verdict: '已用',
    ...over,
  };
}

test('下钻：按协议行分组，作废 / 软删也留着（counted=false）', () => {
  const all = [
    source({}),
    source({ order_id: 'order-2', order_no: 'PO-002', counted: false, verdict: '采购单已作废' }),
    source({ agreement_item_id: 'item-2', order_id: 'order-3', order_no: 'PO-003' }),
  ];
  assert.equal(sourcesOf(all, 'item-1').length, 2);
  assert.equal(sourcesOf(all, 'item-2').length, 1);
  assert.deepEqual(sourcesOf(all, 'item-x'), [], '这一行没被引用过 = 空数组，不是错误');

  assert.equal(sourcesSummaryText(all), '计入 2 行 · 释放 1 行');
  assert.equal(sourcesSummaryText([]), '计入 0 行 · 释放 0 行');
  assert.equal(sourceVerdictTheme({ counted: true }), 'success');
  assert.equal(sourceVerdictTheme({ counted: false }), 'default', '作废是正常业务，别染红');
});

test('下钻行：软删单要写「已删」、主键是「单 + 行号」', () => {
  assert.equal(sourceOrderText({ order_no: 'PO-001', order_deleted_at: null }), 'PO-001');
  assert.equal(sourceOrderText({ order_no: 'PO-001', order_deleted_at: '2026-11-18T10:00:00Z' }), 'PO-001（已删）');
  assert.equal(sourceKey({ order_id: 'o1', line_no: 2 }), 'o1:2');
});

// ==================== ⑦ 分页入参 ====================

test('分页入参：空串不传、trim 关键字、over_only 只在 true 时传', () => {
  assert.deepEqual(buildLedgerQuery(emptyLedgerFilter(), 1, 20), { page: 1, page_size: 20 });
  assert.deepEqual(
    buildExecQuery(emptyExecFilter(), 1, 20),
    { page: 1, page_size: 20 },
    'over_only=false 不传（后端默认 false，传了是噪音）',
  );
  assert.deepEqual(
    buildQuery({ keyword: '  FWA-2026  ', status: 'APPROVED', validity_state: '' }, 3, 50),
    { page: 3, page_size: 50, keyword: 'FWA-2026', status: 'APPROVED' },
  );
  assert.deepEqual(
    buildQuery({ keyword: '', status: '', validity_state: 'EXPIRED', over_only: true }, 1, 20),
    { page: 1, page_size: 20, validity_state: 'EXPIRED', over_only: true },
  );
});

test('分页入参：页码 / 每页条数兜底（0 或 NaN 打到后端会 400）', () => {
  assert.deepEqual(buildQuery(emptyLedgerFilter(), 0, 0), { page: 1, page_size: PAGE_SIZE });
  assert.deepEqual(buildQuery(emptyLedgerFilter(), Number.NaN, Number.NaN), { page: 1, page_size: PAGE_SIZE });
  assert.deepEqual(buildQuery(emptyLedgerFilter(), -3, -8), { page: 1, page_size: 1 });
});

test('关键字超长要先提示（后端 400 也拦，但别让用户猜）', () => {
  assert.equal(keywordWarning({ keyword: 'x'.repeat(MAX_KEYWORD) }), null);
  assert.match(keywordWarning({ keyword: 'x'.repeat(MAX_KEYWORD + 1) }) ?? '', /最长 64 个字符/);
  assert.equal(keywordWarning({ keyword: `  ${'x'.repeat(MAX_KEYWORD)}  ` }), null, '前后空格不算长度');
});

// ==================== ⑧ 空态 / 标题 / 展示回退 ====================

test('空态文案：有筛选 vs 没筛选分开说', () => {
  assert.match(emptyListText(emptyLedgerFilter(), 'ledger'), /还没有框架协议/);
  assert.match(emptyListText(emptyExecFilter(), 'execution'), /执行量/);
  assert.match(emptyListText({ keyword: 'FWA', status: '', validity_state: '' }, 'ledger'), /没有符合筛选条件/);
  assert.match(emptyListText({ keyword: '', status: 'APPROVED', validity_state: '' }, 'execution'), /没有符合筛选条件/);
});

test('抽屉标题：协议号 + 名称；空时给默认标题', () => {
  const row = { agreement_no: 'FWA-20260101-000001', agreement_name: '2026 年钢板框架' };
  assert.equal(detailTitle(row as Parameters<typeof detailTitle>[0]), 'FWA-20260101-000001 2026 年钢板框架');
  assert.equal(detailTitle(null), '协议详情');
});

test('供应商展示用现名（约定 13：业务展示实时取名），现名空了才回退快照', () => {
  assert.equal(supplierText({ supplier_display_name: '宝钢（现名）', supplier_name: '宝钢（签约时）' }), '宝钢（现名）');
  assert.equal(supplierText({ supplier_display_name: '', supplier_name: '宝钢（签约时）' }), '宝钢（签约时）');
  assert.equal(supplierText({ supplier_display_name: '', supplier_name: null }), '—');
});

test('经办：部门 / 人，缺一头只显示另一头', () => {
  assert.equal(ownerText({ dept_name: '采购部', owner_name: '张三' }), '采购部 / 张三');
  assert.equal(ownerText({ dept_name: '采购部', owner_name: null }), '采购部');
  assert.equal(ownerText({ dept_name: null, owner_name: '张三' }), '张三');
  assert.equal(ownerText({ dept_name: null, owner_name: null }), '—');
});
