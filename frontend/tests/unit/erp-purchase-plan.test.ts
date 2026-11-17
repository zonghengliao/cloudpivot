/**
 * 采购计划页面纯逻辑单测（M4-06：录入 + 执行率）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 为什么这些判据要在前端也钉一遍
 *
 * 后端 `purchase_plan_dto.rs` / `purchase_plan_execution_dto.rs` / `purchase_plan_convert_dto.rs`
 * 各自有纯函数单测；前端这一份钉的是**同一批口径在前端不会漂**：
 *
 * | 组 | 钉住什么 |
 * |----|----------|
 * | ① | 类型 × 月份自洽（ANNUAL 不带月 / MONTHLY 必须 1–12），年度 1990–2200 |
 * | ② | 「物料 × 仓库」重复必须在**请求前**拦住，且报文点出第几行撞第几行 |
 * | ③ | 数量 > 0、单价 ≥ 0、明细 1–500 行 |
 * | ④ | 分页入参：空串 / null 一律不传（漏一个 `?keyword=` 就查出 0 行） |
 * | ⑤ | 金额只做**预览**：4 位舍入、与后端 `line_amount` 同向；提交一律字符串 |
 * | ⑥ | 执行率：pct 只加百分号、进度条封顶 100、负数剩余写「超 X」、状态不按 pct 反推 |
 * | ⑦ | 转申请：剩余量 = 计划 − 已执行（负数按 0）、超量拦截、单价 0 必须覆盖 |
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { PlanDetail, PlanRow } from '@/api/business/erp/purchase-plan';
import {
  MAX_LINES,
  MAX_NAME,
  YEAR_MAX,
  YEAR_MIN,
  buildConvertPayload,
  buildExecQuery,
  buildPlanPayload,
  buildQuery,
  convertTotals,
  convertedSummaryText,
  emptyConvertForm,
  emptyExecFilter,
  emptyFilter,
  emptyPlanForm,
  execStateTheme,
  formFromDetail,
  formTotals,
  lineAmountText,
  lineCountsText,
  monthForType,
  newPlanLine,
  parentCandidates,
  pctText,
  periodText,
  planTypeLabel,
  progressPercent,
  remainingScaled,
  remainingText,
  statusLabel,
  statusTheme,
  validateConvert,
  validatePlanForm,
  type PlanForm,
} from '@/views/business/erp/purchase-plan/purchase-plan-logic';

/** 造一行计划（只填判据用得到的字段） */
function planRow(overrides: Partial<PlanRow> = {}): PlanRow {
  return {
    id: 'p0000000-0000-0000-0000-000000000001',
    plan_no: 'PPL-202611-0001',
    plan_name: '2026 年度采购计划',
    plan_type: 'ANNUAL',
    plan_type_label: '年度计划',
    plan_year: 2026,
    plan_month: null,
    parent_plan_id: null,
    parent_plan_no: null,
    dept_id: null,
    dept_name: null,
    planner_id: null,
    planner_name: null,
    status: 'DRAFT',
    status_label: '草稿',
    approval_instance_id: null,
    total_planned_qty: '10.0000',
    total_executed_qty: '0.0000',
    total_planned_amount: '100.0000',
    remark: null,
    item_count: 1,
    created_at: '2026-11-18T00:00:00+08:00',
    create_by: null,
    editable: true,
    ...overrides,
  };
}

/** 一张干净的单行表单（默认合法） */
function formOf(overrides: Partial<PlanForm> = {}): PlanForm {
  const base = emptyPlanForm(2026);
  return {
    ...base,
    plan_name: '2026 年度原材料采购计划',
    lines: [
      { key: 1, material_id: 'm1', warehouse_id: 'w1', planned_qty: '3', planned_price: '2.5', remark: '' },
    ],
    ...overrides,
  };
}

// ==================== ① 类型 × 月份 / 年度 ====================

test('年度计划必须不带月份；月度计划必须 1–12', () => {
  assert.equal(validatePlanForm(formOf({ plan_type: 'ANNUAL', plan_month: null })), null);
  assert.match(
    String(validatePlanForm(formOf({ plan_type: 'ANNUAL', plan_month: 11 }))),
    /年度计划不能带月份/,
  );
  assert.equal(validatePlanForm(formOf({ plan_type: 'MONTHLY', plan_month: 11 })), null);
  assert.match(String(validatePlanForm(formOf({ plan_type: 'MONTHLY', plan_month: null }))), /必须选月份/);
  assert.match(String(validatePlanForm(formOf({ plan_type: 'MONTHLY', plan_month: 13 }))), /必须选月份/);
});

test('年度超范围 1990–2200 必须拒（与 ck_biz_pp_year 同口径）', () => {
  assert.equal(validatePlanForm(formOf({ plan_year: YEAR_MIN })), null);
  assert.equal(validatePlanForm(formOf({ plan_year: YEAR_MAX })), null);
  assert.match(String(validatePlanForm(formOf({ plan_year: YEAR_MIN - 1 }))), /超范围/);
  assert.match(String(validatePlanForm(formOf({ plan_year: YEAR_MAX + 1 }))), /超范围/);
  assert.match(String(validatePlanForm(formOf({ plan_year: '2026.5' }))), /整数/);
});

test('类型切换：年度清空月份，月度自动补当月', () => {
  assert.equal(monthForType('ANNUAL', 11), null);
  assert.equal(monthForType('MONTHLY', 11), 11);
  const month = monthForType('MONTHLY', null);
  assert.ok(month !== null && month >= 1 && month <= 12, '月度必须拿到一个 1–12 的月份');
});

// ==================== ② 父单 / 粒度重复 ====================

test('父单候选只留「同年度 + 年度计划」，且排除自己', () => {
  const plans = [
    planRow({ id: 'a', plan_type: 'ANNUAL', plan_year: 2026 }),
    planRow({ id: 'b', plan_type: 'MONTHLY', plan_year: 2026, plan_month: 3 }),
    planRow({ id: 'c', plan_type: 'ANNUAL', plan_year: 2025 }),
    planRow({ id: 'd', plan_type: 'ANNUAL', plan_year: 2026 }),
  ];
  const picked = parentCandidates(plans, 2026, 'd').map((plan) => plan.id);
  assert.deepEqual(picked, ['a']);
});

test('父单必须是同年度年度计划（跨年 / 月度 → 400 级错误）', () => {
  const annuals = [planRow({ id: 'a', plan_type: 'ANNUAL', plan_year: 2026 })];
  assert.equal(
    validatePlanForm(formOf({ plan_type: 'MONTHLY', plan_month: 3, parent_plan_id: 'a' }), annuals),
    null,
  );
  assert.match(
    String(validatePlanForm(formOf({ plan_type: 'MONTHLY', plan_month: 3, parent_plan_id: 'zzz' }), annuals)),
    /不存在/,
  );
  assert.match(
    String(validatePlanForm(formOf({ plan_type: 'ANNUAL', parent_plan_id: 'a' }), annuals)),
    /只有月度计划能挂/,
  );
  // 跨年：父单是 2025 年，本单 2026
  const crossYear = [planRow({ id: 'c', plan_type: 'ANNUAL', plan_year: 2025 })];
  assert.match(
    String(validatePlanForm(formOf({ plan_type: 'MONTHLY', plan_month: 3, parent_plan_id: 'c' }), crossYear)),
    /不一致/,
  );
});

test('同一张计划里「物料 × 仓库」重复：报文点出第几行撞第几行', () => {
  const form = formOf({
    lines: [
      { key: 1, material_id: 'm1', warehouse_id: 'w1', planned_qty: '1', planned_price: '1', remark: '' },
      { key: 2, material_id: 'm1', warehouse_id: 'w2', planned_qty: '1', planned_price: '1', remark: '' },
      { key: 3, material_id: 'm1', warehouse_id: 'w1', planned_qty: '1', planned_price: '1', remark: '' },
    ],
  });
  const error = validatePlanForm(form);
  assert.ok(error, '重复必须拒');
  assert.match(error, /第 3 行与第 1 行/);
  // 换仓库就是合法两行
  assert.equal(
    validatePlanForm(
      formOf({
        lines: [
          { key: 1, material_id: 'm1', warehouse_id: 'w1', planned_qty: '1', planned_price: '1', remark: '' },
          { key: 2, material_id: 'm1', warehouse_id: 'w2', planned_qty: '1', planned_price: '1', remark: '' },
        ],
      }),
    ),
    null,
  );
});

// ==================== ③ 数量 / 单价 / 行数 ====================

test('计划量必须 > 0，单价不能为负，物料仓库必填', () => {
  const line = (over: Partial<PlanForm['lines'][number]>) => formOf({ lines: [{ ...newPlanLine(), material_id: 'm1', warehouse_id: 'w1', planned_qty: '1', planned_price: '1', ...over }] });
  assert.match(String(validatePlanForm(line({ planned_qty: 0 }))), /必须 > 0/);
  assert.match(String(validatePlanForm(line({ planned_qty: -1 }))), /必须 > 0/);
  assert.match(String(validatePlanForm(line({ planned_qty: '' }))), /不能为空/);
  assert.match(String(validatePlanForm(line({ planned_price: -0.01 }))), /不能为负/);
  assert.match(String(validatePlanForm(line({ material_id: '' }))), /还没选物料/);
  assert.match(String(validatePlanForm(line({ warehouse_id: '' }))), /还没选仓库/);
  // 单价 0 合法（计划行可以没有单价，转申请时再补）
  assert.equal(validatePlanForm(line({ planned_price: 0 })), null);
});

test('明细 1–500 行；空明细与超 500 行都拒', () => {
  assert.match(String(validatePlanForm(formOf({ lines: [] }))), /不能为空/);
  const many = Array.from({ length: MAX_LINES + 1 }, (_, index) => ({
    key: index + 1,
    material_id: `m${index}`,
    warehouse_id: 'w1',
    planned_qty: '1',
    planned_price: '1',
    remark: '',
  }));
  assert.match(String(validatePlanForm(formOf({ lines: many }))), new RegExp(`最多 ${MAX_LINES} 行`));
  assert.match(String(validatePlanForm(formOf({ plan_name: '长'.repeat(MAX_NAME + 1) }))), /最长/);
});

// ==================== ④ 分页入参 ====================

test('计划分页入参：空串 / null 一律不传，只剩 page+page_size', () => {
  assert.deepEqual(buildQuery(emptyFilter(), 2, 50), { page: 2, page_size: 50 });
  assert.deepEqual(
    buildQuery({ keyword: '  PPL  ', status: 'SUBMITTED', plan_type: 'MONTHLY', plan_year: 2026, dept_id: 'd1' }, 1),
    {
      page: 1,
      page_size: 20,
      keyword: 'PPL',
      status: 'SUBMITTED',
      plan_type: 'MONTHLY',
      plan_year: 2026,
      dept_id: 'd1',
    },
  );
});

test('执行率分页入参：min_rate = 0 视同不限（不传）；月份 / 年度按数字传', () => {
  assert.deepEqual(buildExecQuery(emptyExecFilter(), 1), { page: 1, page_size: 20 });
  assert.deepEqual(
    buildExecQuery({ ...emptyExecFilter(), plan_year: 2026, plan_month: 11, min_rate: 0 }, 1),
    { page: 1, page_size: 20, plan_year: 2026, plan_month: 11 },
  );
  assert.deepEqual(
    buildExecQuery({ ...emptyExecFilter(), min_rate: 100, keyword: ' 年度 ' }, 3, 10),
    { page: 3, page_size: 10, keyword: '年度', min_rate: 100 },
  );
});

// ==================== ⑤ 金额 / 数量（字符串提交 + 4 位舍入） ====================

test('行金额预览 4 位舍入：3 × 2.5 = 7.5000；0.3333 × 3 = 0.9999', () => {
  assert.equal(lineAmountText(3, 2.5), '7.5000');
  assert.equal(lineAmountText('0.3333', 3), '0.9999');
  assert.equal(lineAmountText('', ''), '0.0000');
});

test('提交入参：数量 4 位、单价 6 位，一律字符串；年度计划不带 plan_month', () => {
  const payload = buildPlanPayload(
    formOf({
      plan_name: '  2026 年度计划  ',
      remark: '  ',
      dept_id: '',
      lines: [
        { key: 1, material_id: 'm1', warehouse_id: 'w1', planned_qty: 3, planned_price: 2.5, remark: ' 行备注 ' },
      ],
    }),
  );
  assert.equal(payload.plan_name, '2026 年度计划');
  assert.equal(payload.plan_month, null);
  assert.equal(payload.remark, null);
  assert.equal(payload.dept_id, null);
  assert.deepEqual(payload.items, [
    { material_id: 'm1', warehouse_id: 'w1', planned_qty: '3.0000', planned_price: '2.500000', remark: '行备注' },
  ]);
});

test('表单合计与后端「Σ 明细」同口径（行数 / 数量 / 金额）', () => {
  const totals = formTotals(
    formOf({
      lines: [
        { key: 1, material_id: 'm1', warehouse_id: 'w1', planned_qty: '3', planned_price: '2.5', remark: '' },
        { key: 2, material_id: 'm2', warehouse_id: 'w1', planned_qty: '0.3333', planned_price: '3', remark: '' },
      ],
    }),
  );
  assert.deepEqual(totals, { lines: 2, qty: '3.3333', amount: '8.4999' });
});

test('详情 → 表单 → 入参 往返：数量/单价字符串化，月份保持', () => {
  const detail: PlanDetail = {
    plan: planRow({ plan_type: 'MONTHLY', plan_month: 11, dept_id: 'd1', planner_id: 'u1', remark: '备注' }),
    items: [
      {
        id: 'i1',
        line_no: 1,
        material_id: 'm1',
        material_code: 'M-001',
        material_name: '钢板',
        warehouse_id: 'w1',
        warehouse_name: '主仓',
        unit_id: 'u',
        unit_name: '吨',
        planned_qty: '12.5000',
        executed_qty: '0.0000',
        planned_price: '3300.000000',
        planned_amount: '41250.0000',
        remark: '行备注',
      },
    ],
  };
  const form = formFromDetail(detail);
  assert.equal(form.id, detail.plan.id);
  assert.equal(form.plan_month, 11);
  assert.equal(form.lines[0].planned_qty, '12.5000');
  assert.equal(form.lines[0].planned_price, '3300.000000');
  const payload = buildPlanPayload(form);
  assert.equal(payload.plan_month, 11);
  assert.equal(payload.items[0].planned_qty, '12.5000');
  assert.equal(payload.items[0].planned_price, '3300.000000');
});

// ==================== ⑥ 执行率展示 ====================

test('执行率百分比只加百分号；空值给 —', () => {
  assert.equal(pctText('40.00'), '40.00%');
  assert.equal(pctText('100.00'), '100.00%');
  assert.equal(pctText(null), '—');
  assert.equal(pctText(undefined), '—');
});

test('进度条封顶 100（超计划在文案里说），空率给 0', () => {
  assert.equal(progressPercent('40.00'), 40);
  assert.equal(progressPercent('99.99'), 99.99);
  assert.equal(progressPercent('150.00'), 100, '超计划封顶，真实倍数看文案');
  assert.equal(progressPercent(null), 0);
  assert.equal(progressPercent('0.00'), 0);
});

test('剩余量为负明确写成「超 X」；余额正常原样', () => {
  assert.equal(remainingText('60.0000'), '60.0000');
  assert.equal(remainingText('-15.0000'), '超 15');
  assert.equal(remainingText(null), '—');
});

test('执行率状态主题四态 + 行状态计数文案', () => {
  assert.equal(execStateTheme('NOT_STARTED'), 'default');
  assert.equal(execStateTheme('PARTIAL'), 'warning');
  assert.equal(execStateTheme('DONE'), 'success');
  assert.equal(execStateTheme('OVER'), 'danger');
  assert.equal(execStateTheme('WEIRD'), 'default');
  assert.equal(
    lineCountsText({ not_started_lines: 3, partial_lines: 2, done_lines: 1, over_lines: 0 }),
    '未开工 3 · 部分 2 · 下满 1 · 超 0',
  );
});

test('期间 / 状态 / 类型文案：未知值原样返回，不吞信息', () => {
  assert.equal(periodText({ plan_year: 2026, plan_month: null }), '2026 年');
  assert.equal(periodText({ plan_year: 2026, plan_month: 11 }), '2026 年 11 月');
  assert.equal(statusLabel('DRAFT'), '草稿');
  assert.equal(statusLabel('CLOSED'), 'CLOSED');
  assert.equal(statusTheme('APPROVED'), 'success');
  assert.equal(planTypeLabel('MONTHLY'), '月度计划');
  assert.equal(planTypeLabel('WEEKLY'), 'WEEKLY');
});

// ==================== ⑦ 转采购申请 ====================

/** 造一张「已批准 + 两行明细」的计划详情（第 2 行单价为 0，转申请时须覆盖） */
function approvedDetail(): PlanDetail {
  return {
    plan: planRow({ status: 'APPROVED', dept_id: 'd1', editable: false }),
    items: [
      {
        id: 'i1',
        line_no: 1,
        material_id: 'm1',
        material_code: 'M-001',
        material_name: '钢板',
        warehouse_id: 'w1',
        warehouse_name: '主仓',
        unit_id: 'u',
        unit_name: '吨',
        planned_qty: '10.0000',
        executed_qty: '4.0000',
        planned_price: '100.000000',
        planned_amount: '1000.0000',
        remark: null,
      },
      {
        id: 'i2',
        line_no: 2,
        material_id: 'm2',
        material_code: 'M-002',
        material_name: '铝锭',
        warehouse_id: 'w2',
        warehouse_name: '备件仓',
        unit_id: 'u',
        unit_name: '吨',
        planned_qty: '5.0000',
        executed_qty: '0.0000',
        planned_price: '0.000000',
        planned_amount: '0.0000',
        remark: null,
      },
    ],
  };
}

test('剩余可转量 = 计划 − 已执行（负数按 0，与 ConvertibleLine::remaining 同口径）', () => {
  assert.equal(remainingScaled('10.0000', '4.0000'), 60_000);
  assert.equal(remainingScaled('10.0000', '10.0000'), 0);
  assert.equal(remainingScaled('10.0000', '12.0000'), 0, '超转（库被改过）按 0，不给负数可转');
});

test('转申请表单缺省：逐行剩余量、单价 0 的行留空等覆盖、公司级预填部门', () => {
  const form = emptyConvertForm(approvedDetail());
  assert.equal(form.dept_id, 'd1');
  assert.equal(form.lines[0].remaining, '6.0000');
  assert.equal(form.lines[0].qty, '6.0000');
  assert.equal(form.lines[0].est_price, '100.000000');
  assert.equal(form.lines[1].remaining, '5.0000');
  assert.equal(form.lines[1].est_price, '', '计划单价 0 → 必须让用户填');
  assert.equal(form.lines[0].selected, true);
});

test('转申请校验：公司级缺部门 / 未勾选 / 超剩余量 / 单价 0 都拒', () => {
  const form = emptyConvertForm(approvedDetail());

  assert.match(String(validateConvert({ ...form, dept_id: '' }, true)), /必须选一个申请部门/);

  // 有编制部门时不必再选：把「计划单价为 0」那行的剩余取消勾选，剩下的就是合法单
  const withDept = { ...form, dept_id: '' };
  withDept.lines = form.lines.map((line, index) => ({ ...line, selected: index === 0 }));
  assert.equal(validateConvert(withDept, false), null, '有编制部门时不必再选');

  const none = emptyConvertForm(approvedDetail());
  none.lines.forEach((line) => (line.selected = false));
  assert.match(String(validateConvert(none, false)), /至少勾选一行/);

  const over = emptyConvertForm(approvedDetail());
  over.lines[0].qty = '6.0001';
  assert.match(String(validateConvert(over, false)), /第 1 行.*超过剩余可转量 6.0000/);

  const zeroPrice = emptyConvertForm(approvedDetail());
  assert.match(String(validateConvert(zeroPrice, false)), /第 2 行.*必须 > 0/);

  const zeroQty = emptyConvertForm(approvedDetail());
  zeroQty.lines[0].qty = '0';
  assert.match(String(validateConvert(zeroQty, false)), /第 1 行.*必须 > 0/);
});

test('转申请入参：只送勾选行，数量 4 位 / 单价 6 位，税率 4 位', () => {
  const form = emptyConvertForm(approvedDetail());
  form.lines[1].selected = false;
  form.lines[0].qty = '2.5';
  form.tax_rate = 13;
  form.remark = '  分批转  ';
  const payload = buildConvertPayload(form);
  assert.equal(payload.dept_id, 'd1');
  assert.equal(payload.tax_rate, '13.0000');
  assert.equal(payload.remark, '分批转');
  assert.equal(payload.req_date, null);
  assert.deepEqual(payload.items, [{ plan_item_id: 'i1', qty: '2.5000', est_price: '100.000000' }]);
});

test('转申请预览合计：只算勾选行（数量与 4 位金额）', () => {
  const form = emptyConvertForm(approvedDetail());
  form.lines[1].est_price = '200';
  const totals = convertTotals(form);
  assert.deepEqual(totals, { lines: 2, qty: '11.0000', amount: '1600.0000' });
  form.lines[1].selected = false;
  assert.deepEqual(convertTotals(form), { lines: 1, qty: '6.0000', amount: '600.0000' });
});

test('转申请结果一句话：N 张申请单（单号 + 仓库）；空结果有人说人话', () => {
  const text = convertedSummaryText([
    {
      requisition_id: 'r1',
      req_no: 'PRQ-20261118-000001',
      warehouse_id: 'w1',
      warehouse_name: '主仓',
      dept_id: 'd1',
      dept_name: '采购部',
      req_date: '2026-11-18',
      est_amount: '600.0000',
      line_count: 1,
      lines: [],
    },
  ]);
  assert.match(text, /PRQ-20261118-000001（主仓）/);
  assert.equal(convertedSummaryText([]), '没有生成申请单');
});
