/**
 * 佣金两页的纯逻辑单测（SO-37 / ARCH-7 §15.4）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * # 为什么测 `.ts` 不测 `.vue`
 *
 * 「费率是百分数还是小数」「`ALL` 能不能带 `scope_id`」「哪些状态能点审批 / 发放」
 * 「生成请求发了什么」全在 `rule-logic.ts` / `commission-logic.ts` 里，`.vue` 只渲染。
 * 这些一旦漂移 `vue-tsc` 抓不到（编译期两边都是 `string`），而线上表现是**算错钱**：
 * 把 5% 当 5 元发出去、或者 409 批量弹。
 *
 * # 断言口径照后端 / 迁移抄，不照卡面
 *
 * - `backend/src/business/erp/commission_dto.rs`：`STATUSES` / `LEGAL_EDGES` / 期间校验
 * - `backend/migrations/business/V0309__sales_commission.up.sql`：三条金额 / 范围 CHECK
 * - `backend/migrations/business/V0310__commission_permissions.up.sql`：7 个权限码
 *
 * 其中「源码即契约」两条（状态边、权限码）直接 `readFileSync` 读后端文件断言逐字包含，
 * 后端改字面量 / 前端抄错都会当场红（本仓 `flow-biz-link.test.ts` 同款做法）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

import {
  amountRangeText,
  buildRulePayload,
  calcTypeLabel,
  emptyRuleFilter,
  emptyRuleForm,
  isRuleFiltered,
  moneyText,
  onCalcTypeChange,
  onScopeTypeChange,
  ratePlaceholder,
  rateSuffix,
  rateText,
  ruleEmptyText,
  ruleFormFrom,
  ruleStatusLabel,
  scopeDisplay,
  scopeTypeLabel,
  toPlain,
  validateRuleForm,
  validityText,
  RULE_NAME_MAX,
  type RuleFormDraft,
} from '@/views/business/erp/commission-rule/rule-logic';

import type {
  CommissionRuleRow,
  CommissionSettlementRow,
  SettleCommissionRes,
} from '@/api/business/erp/commission';

import {
  COMMISSION_STATUS_EDGES,
  buildSettlePayload,
  canTransition,
  emptySettlementFilter,
  isClawback,
  isItemClawback,
  isSettlementFiltered,
  itemRateText,
  nextStatuses,
  pageTotals,
  paidAtText,
  periodLabel,
  periodOptions,
  settlementEmptyText,
  settlementStatusLabel,
  settleResultText,
  skippedText,
  statusActionLabel,
  statusNeedsReason,
  validateSettlePeriod,
  voucherText,
} from '@/views/business/erp/commission/commission-logic';

const HERE = dirname(fileURLToPath(import.meta.url));
/** 仓库根（tests/unit → frontend → 仓库根） */
const REPO_ROOT = resolve(HERE, '..', '..', '..');

function rule(overrides: Partial<CommissionRuleRow> = {}): CommissionRuleRow {
  return {
    id: 'c0a10000-0000-4000-8000-000000000101',
    rule_name: '华东销售 5%',
    scope_type: 'SALESPERSON',
    scope_id: 'c0a10000-0000-4000-8000-0000000000u1'.replace('u', 'a'),
    calc_type: 'PERCENT',
    rate: '5.0000',
    min_amount: '0.0000',
    max_amount: null,
    effective_from: '2026-11-01',
    effective_to: null,
    status: 'ACTIVE',
    remark: null,
    created_at: '2026-11-01T00:00:00Z',
    updated_at: '2026-11-01T00:00:00Z',
    ...overrides,
  };
}

function settlement(overrides: Partial<CommissionSettlementRow> = {}): CommissionSettlementRow {
  return {
    id: 'c0a20000-0000-4000-8000-000000000201',
    settlement_no: 'CM-202611-000001',
    salesperson_id: 'c0a10000-0000-4000-8000-0000000000a1',
    salesperson_name: '张三',
    period_code: '2026-11',
    total_sales: '1000.0000',
    total_commission: '50.0000',
    status: 'DRAFT',
    paid_at: null,
    voucher_id: null,
    voucher_no: null,
    item_count: 2,
    remark: null,
    created_at: '2026-11-30T00:00:00Z',
    ...overrides,
  };
}

// ==================== ① 规则表单校验（库层 CHECK 的前端镜像） ====================

test('规则表单：必填 / 范围对象 / 费率 / 金额区间 / 有效期逐条拦', () => {
  const base = emptyRuleForm('2026-11-07');
  base.rule_name = '华东销售 5%';
  base.scope_id = 'u-1';
  base.rate = '5';

  assert.equal(validateRuleForm(base), null, '基线表单应通过');

  // ① 规则名
  assert.match(validateRuleForm({ ...base, rule_name: '  ' }) ?? '', /规则名/);
  assert.match(validateRuleForm({ ...base, rule_name: 'x'.repeat(RULE_NAME_MAX + 1) }) ?? '', /最多/);

  // ② ALL 不许带 scope_id（ck_biz_cmrule_scope_id）
  assert.match(
    validateRuleForm({ ...base, scope_type: 'ALL', scope_id: 'u-1' }) ?? '',
    /必须为空/,
    'ALL 带 scope_id 会被库层 CHECK 拒，前端先拦',
  );
  assert.equal(validateRuleForm({ ...base, scope_type: 'ALL', scope_id: '' }), null);

  // ③ 非 ALL 必须绑对象
  assert.match(validateRuleForm({ ...base, scope_id: '' }) ?? '', /请选择销售员/);
  assert.match(
    validateRuleForm({ ...base, scope_type: 'MATERIAL_CATEGORY', scope_id: '' }) ?? '',
    /请选择物料/,
  );

  // ④ PERCENT 上界 100；负数一律不许（percent / fixed 同）
  assert.match(validateRuleForm({ ...base, rate: '100.0001' }) ?? '', /不能超过 100/);
  assert.equal(validateRuleForm({ ...base, rate: '100' }), null);
  assert.match(validateRuleForm({ ...base, rate: '-1' }) ?? '', /不能为负/);
  // FIXED 可以远大于 100（这是金额不是百分数）
  assert.equal(validateRuleForm({ ...base, calc_type: 'FIXED', rate: '5000' }), null);
  assert.match(validateRuleForm({ ...base, rate: '' }) ?? '', /请填写费率/);

  // ⑤ 起算 / 封顶（ck_biz_cmrule_amount：min ≥ 0 且 max ≥ min）
  assert.match(validateRuleForm({ ...base, min_amount: '-1' }) ?? '', /起算金额/);
  assert.match(
    validateRuleForm({ ...base, min_amount: '100', max_amount: '99' }) ?? '',
    /封顶金额不能小于起算金额/,
  );
  assert.equal(validateRuleForm({ ...base, min_amount: '100', max_amount: '100' }), null);

  // ⑥ 有效期（ck_biz_cmrule_effective：to ≥ from）
  assert.match(
    validateRuleForm({ ...base, effective_from: '2026-11-30', effective_to: '2026-11-01' }) ?? '',
    /不能早于/,
  );
  assert.match(validateRuleForm({ ...base, effective_from: '' }) ?? '', /生效起始日/);
  assert.equal(validateRuleForm({ ...base, effective_to: '2026-11-30' }), null, '同一天合法（两端都含）');
});

test('规则表单：切范围清 scope_id、切计算方式清 rate（防 5% 变 5 元）', () => {
  const form: RuleFormDraft = { ...emptyRuleForm('2026-11-07'), scope_id: 'u-1', rate: '5' };

  onScopeTypeChange(form, 'ALL');
  assert.equal(form.scope_type, 'ALL');
  assert.equal(form.scope_id, '', 'ALL 不允许带 scope_id（库层 CHECK）');

  onScopeTypeChange(form, 'MATERIAL_CATEGORY');
  assert.equal(form.scope_id, '', '换回具体范围也不保留上一个人的 id');

  onCalcTypeChange(form, 'FIXED');
  assert.equal(form.rate, '', 'PERCENT 的 5 是 5%，FIXED 的 5 是 5 元 —— 必须清空重填');

  // 同一类型重复点不该清掉用户输入
  const fixedForm: RuleFormDraft = { ...emptyRuleForm('2026-11-07'), calc_type: 'FIXED', rate: '50' };
  onCalcTypeChange(fixedForm, 'FIXED');
  assert.equal(fixedForm.rate, '50');
});

test('规则入参：ALL ⇒ scope_id null；空封顶 / 空截止日 ⇒ null；名字去空格', () => {
  const form: RuleFormDraft = {
    ...emptyRuleForm('2026-11-07'),
    rule_name: '  全局兜底  ',
    scope_type: 'ALL',
    scope_id: 'u-ignored',
    rate: '3',
    min_amount: '0',
    max_amount: '',
    effective_to: '',
    remark: '   ',
  };
  const payload = buildRulePayload(form);
  assert.deepEqual(payload, {
    rule_name: '全局兜底',
    scope_type: 'ALL',
    scope_id: null,
    calc_type: 'PERCENT',
    rate: '3',
    min_amount: '0',
    max_amount: null,
    effective_from: '2026-11-07',
    effective_to: null,
    status: 'ACTIVE',
    remark: null,
  });

  // 金额走字符串（后端 NUMERIC），且不做浮点运算 —— '0.1000' 原样给 0.1
  const fixed = buildRulePayload({
    ...form,
    scope_type: 'MATERIAL_CATEGORY',
    scope_id: 'm-1',
    calc_type: 'FIXED',
    rate: '0.1',
    max_amount: '99.9999',
    effective_to: '2027-01-31',
  });
  assert.equal(fixed.rate, '0.1');
  assert.equal(fixed.scope_id, 'm-1');
  assert.equal(fixed.max_amount, '99.9999');
  assert.equal(fixed.effective_to, '2027-01-31');
});

test('规则回填：列表行 → 表单（4 位小数去掉尾零，null 折空串）', () => {
  const form = ruleFormFrom(rule({ rate: '5.0000', min_amount: '100.0000', max_amount: null }));
  assert.equal(form.rate, '5');
  assert.equal(form.min_amount, '100');
  assert.equal(form.max_amount, '');
  assert.equal(form.effective_to, '');

  assert.equal(toPlain('1.5000'), '1.5');
  assert.equal(toPlain('1.0000'), '1');
  assert.equal(toPlain(null), '');
});

// ==================== ② 规则展示口径 ====================

test('展示文案：PERCENT 带 %、FIXED 带「元/单」，别让人把 5 当 5 元', () => {
  assert.equal(rateText(rule({ calc_type: 'PERCENT', rate: '5.0000' })), '5.0000%');
  assert.equal(rateText(rule({ calc_type: 'FIXED', rate: '50.0000' })), '50.00 元/单');
  assert.equal(
    amountRangeText({ min_amount: '0.0000', max_amount: null }),
    '0.00 / 不封顶',
    '封顶为空要写「不封顶」，别留白',
  );
  assert.equal(
    amountRangeText({ min_amount: '100.0000', max_amount: '500.0000' }),
    '100.00 / 500.00',
  );
  assert.equal(validityText('2026-11-01', null), '2026-11-01 ~ 长期');
  assert.equal(validityText('2026-11-01', '2026-12-31'), '2026-11-01 ~ 2026-12-31');

  assert.equal(scopeTypeLabel('SALESPERSON'), '按销售员');
  assert.equal(scopeTypeLabel('MATERIAL_CATEGORY'), '按物料');
  assert.equal(scopeTypeLabel('ALL'), '全局');
  assert.equal(scopeTypeLabel('WHAT'), 'WHAT', '未知枚举回落编码，别显示空白');
  assert.equal(calcTypeLabel('PERCENT'), '百分比');
  assert.equal(ruleStatusLabel('INACTIVE'), '停用');

  assert.equal(ratePlaceholder('PERCENT'), '百分数，例如 5 表示 5%');
  assert.equal(rateSuffix('FIXED'), '元/单');
  assert.equal(moneyText('-40.0000'), '-40.00', '冲回金额必须留住负号');

  // 范围对象：有名字用名字，没名字用短 id，ALL 说明「无范围限制」
  assert.equal(scopeDisplay(rule({ scope_type: 'ALL', scope_id: null })), '全部（无范围限制）');
  assert.equal(scopeDisplay(rule({ scope_name: '张三' })), '张三');
  assert.equal(
    scopeDisplay(rule({ scope_id: 'c0a10000-0000-4000-8000-0000000000a1', scope_name: null })),
    '销售员 c0a10000…',
  );
});

test('筛选 / 空态：默认不带条件，有空态文案区分「没数据」与「筛空了」', () => {
  const filter = emptyRuleFilter();
  assert.equal(isRuleFiltered(filter), false);
  assert.match(ruleEmptyText(filter), /还没有佣金规则/);

  const filtered = { ...filter, status: 'INACTIVE' as const };
  assert.equal(isRuleFiltered(filtered), true);
  assert.match(ruleEmptyText(filtered), /没有匹配/);

  const settlementFilter = emptySettlementFilter();
  assert.equal(isSettlementFiltered(settlementFilter), false);
  assert.match(settlementEmptyText(settlementFilter), /生成结算单/);
  assert.match(
    settlementEmptyText({ ...settlementFilter, period_code: '2026-11' }),
    /没有匹配/,
  );
});

// ==================== ③ 结算状态机（与后端 LEGAL_EDGES 同口径） ====================

test('结算状态机：只能 DRAFT→APPROVED→PAID，没有回退边', () => {
  assert.equal(canTransition('DRAFT', 'APPROVED'), true);
  assert.equal(canTransition('DRAFT', 'PAID'), false, '跳步 = 钱没计提就发放');
  assert.equal(canTransition('APPROVED', 'PAID'), true);
  assert.equal(canTransition('APPROVED', 'DRAFT'), false, '回退 = 改写已入账凭证');
  assert.equal(canTransition('PAID', 'APPROVED'), false);
  assert.equal(canTransition('PAID', 'DRAFT'), false);

  assert.deepEqual(nextStatuses('DRAFT'), ['APPROVED']);
  assert.deepEqual(nextStatuses('APPROVED'), ['PAID']);
  assert.deepEqual(nextStatuses('PAID'), [], 'PAID 是终态');
  assert.deepEqual(nextStatuses(null), []);

  assert.equal(statusActionLabel('APPROVED'), '审批通过');
  assert.equal(statusActionLabel('PAID'), '确认发放');
  assert.equal(statusNeedsReason('PAID'), true, '发放要说明（可空，但输入框要在）');
  assert.equal(statusNeedsReason('APPROVED'), false);

  assert.equal(settlementStatusLabel('DRAFT'), '草稿');
  assert.equal(settlementStatusLabel('PAID'), '已发放');
  assert.equal(settlementStatusLabel('WHATEVER'), 'WHATEVER');
});

test('前端状态边 = 后端 commission_dto::LEGAL_EDGES（源码即契约）', () => {
  const dto = readFileSync(
    resolve(REPO_ROOT, 'backend/src/business/erp/commission_dto.rs'),
    'utf8',
  );
  assert.ok(
    dto.includes('pub const LEGAL_EDGES: [(&str, &str); 2] = [\n    (STATUS_DRAFT, STATUS_APPROVED),\n    (STATUS_APPROVED, STATUS_PAID),\n];'),
    '后端 LEGAL_EDGES 变了：前端 COMMISSION_STATUS_EDGES 必须同步',
  );
  assert.deepEqual(COMMISSION_STATUS_EDGES, [
    ['DRAFT', 'APPROVED'],
    ['APPROVED', 'PAID'],
  ]);
  // 状态域三值，顺序一致
  assert.ok(dto.includes('pub const STATUSES: [&str; 3] = [STATUS_DRAFT, STATUS_APPROVED, STATUS_PAID];'));
});

test('权限码 = V0310 种的 7 个（前端先核对，别再出现「卡面写错码」）', () => {
  const migration = readFileSync(
    resolve(REPO_ROOT, 'backend/migrations/business/V0310__commission_permissions.up.sql'),
    'utf8',
  );
  for (const code of [
    'business:erp:commission-rule:list',
    'business:erp:commission-rule:create',
    'business:erp:commission-rule:update',
    'business:erp:commission-rule:delete',
    'business:erp:commission-settle:list',
    'business:erp:commission-settle:create',
    'business:erp:commission-settle:status',
  ]) {
    assert.ok(migration.includes(`'${code}'`), `V0310 里没有 ${code}（前端 PERM 抄错了？）`);
  }
});

// ==================== ④ 生成结算单 ====================

test('生成入参：销售员留空 = 全员（字段整个不发，不是发空串）', () => {
  assert.deepEqual(buildSettlePayload('2026-11'), { period_code: '2026-11' });

  const one = buildSettlePayload('2026-11', 'u-1', '  工资一并发放  ');
  assert.deepEqual(one, { period_code: '2026-11', salesperson_id: 'u-1', remark: '工资一并发放' });

  // 空白备注不发（后端会把它写进结算单备注，空串等于「有一条空备注」）
  assert.deepEqual(buildSettlePayload('2026-11', undefined, '   '), { period_code: '2026-11' });
});

test('期间校验：yyyy-MM，月份 01–12（与 ck_biz_cmset_period 同口径）', () => {
  assert.equal(validateSettlePeriod('2026-11'), null);
  assert.match(validateSettlePeriod('') ?? '', /请选择/);
  assert.match(validateSettlePeriod('2026-13') ?? '', /yyyy-MM/);
  assert.match(validateSettlePeriod('2026-00') ?? '', /yyyy-MM/);
  assert.match(validateSettlePeriod('202611') ?? '', /yyyy-MM/);
});

test('期间候选：本月往前 12 个月（13 个，无未来月），跨年正确', () => {
  const options = periodOptions(new Date('2026-11-07T04:00:00Z')); // 北京时间 2026-11-07 12:00
  assert.equal(options.length, 13);
  assert.deepEqual(options[0], { label: '2026 年 11 月', value: '2026-11' });
  assert.deepEqual(options[1], { label: '2026 年 10 月', value: '2026-10' });
  assert.deepEqual(options[2], { label: '2026 年 9 月', value: '2026-09' }, '月份要补零');
  assert.deepEqual(options[12], { label: '2025 年 11 月', value: '2025-11' }, '跨年要正确');
  assert.equal(options.every((item) => item.value <= '2026-11'), true, '不许出现未来期间');

  assert.equal(periodLabel('2026-11'), '2026 年 11 月');
  assert.equal(periodLabel('bad'), 'bad');
  assert.equal(periodLabel(null), '—');
});

test('生成结果文案：生成几张 + 谁被跳过（跳过是 200，不是错误）', () => {
  const res: SettleCommissionRes = {
    period_code: '2026-11',
    settlements: [settlement(), settlement({ id: 'x', settlement_no: 'CM-202611-000002' })],
    skipped: [
      {
        salesperson_id: 'u-2',
        salesperson_name: '李四',
        reason: '该期间没有新的已收款，也没有退货冲回',
      },
    ],
  };
  const text = settleResultText(res);
  assert.match(text, /已生成 2 张草稿结算单/);
  assert.match(text, /2026 年 11 月/);
  assert.match(text, /1 位销售员被跳过/);
  assert.equal(skippedText(res.skipped[0]!), '李四：该期间没有新的已收款，也没有退货冲回');

  const empty = settleResultText({ period_code: '2026-11', settlements: [], skipped: [] });
  assert.match(empty, /没有生成结算单/);
});

// ==================== ⑤ 结算展示 / 汇总 ====================

test('冲回口径：负数佣金要能识别出来（页面标红 + 「冲回」标签）', () => {
  assert.equal(isClawback('-40.0000'), true);
  assert.equal(isClawback('0.0000'), false);
  assert.equal(isClawback(null), false);
  assert.equal(isItemClawback({ base_amount: '-400.0000', commission: '-40.0000' }), true);
  assert.equal(isItemClawback({ base_amount: '-400.0000', commission: '0.0000' }), true);
  assert.equal(isItemClawback({ base_amount: '400.0000', commission: '40.0000' }), false);

  assert.equal(itemRateText({ rate: '20.0000' }), '20', '明细比率去尾零，别显示 20.0000');
  assert.equal(itemRateText({ rate: '2.5000' }), '2.5');
});

test('本页合计：只汇总当前页（页面上必须写「本页」，别被当全量对账）', () => {
  const totals = pageTotals([
    settlement({ total_sales: '1000.0000', total_commission: '50.0000' }),
    settlement({ id: 'b', total_sales: '0.0000', total_commission: '-40.0000' }),
    settlement({ id: 'c', total_sales: '500.0000', total_commission: '30.0000' }),
  ]);
  assert.equal(totals.count, 3);
  assert.equal(totals.sales, 1500);
  assert.equal(totals.commission, 40, '50 - 40 + 30，负号要算进去');
});

test('时间 / 凭证文案：无凭证写「未计提」，不是空白', () => {
  assert.equal(paidAtText('2026-11-30T10:00:00Z'), '2026-11-30');
  assert.equal(paidAtText(null), '—');
  assert.equal(voucherText({ voucher_no: 'PZ-20261130-000001', voucher_id: 'v-1' }), 'PZ-20261130-000001');
  assert.equal(voucherText({ voucher_no: null, voucher_id: 'c0a90000-0000-4000-8000-000000000901' }), '凭证 c0a90000…');
  assert.equal(voucherText({ voucher_no: null, voucher_id: null }), '—（未计提）');
});
