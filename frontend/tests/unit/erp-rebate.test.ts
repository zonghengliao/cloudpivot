/**
 * 销售返利两 tab 的纯逻辑单测（M3-19 前端 / 后端 M3-15、M3-16、M3-17）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * # 为什么测 `.ts` 不测 `.vue`
 *
 * 「按量能不能给 3 档」「阶梯中间断档」「百分比 2 还是 0.02」「期间码与粒度配错」
 * 「跑批请求发了什么」全在 `policy-logic.ts` / `settle-logic.ts` 里，`.vue` 只渲染。
 * 这些一旦漂移 `vue-tsc` 抓不到（编译期两边都是 `string`），而线上表现是**算错钱**
 * 或者「点了提交才 400」。
 *
 * # 断言口径照后端 / 迁移抄，不照卡面
 *
 * - `backend/app/src/business/erp/sales/rebate_dto.rs`：`RULE_TYPES` / `RATE_UNITS` /
 *   `SETTLE_PERIODS` / `MAX_TIERS` / `MIN_TIER_TIERS` / `checked_rules` 的形状规则
 * - `backend/app/src/business/erp/sales/rebate_settle_dto.rs`：`STATUSES` / `PeriodArgs::resolve`
 * - `backend/app/migrations/business/V0438~V0440`：8 个权限码
 * - `backend/app/migrations/business/V0443`：菜单 path / component / title 与本页路由逐字一致
 *
 * 其中「源码即契约」三条（域值、权限码、菜单 path）直接 `readFileSync` 读后端文件
 * 断言逐字包含 —— 后端改字面量 / 前端抄错都会当场红（本仓 `erp-commission.test.ts` 同款做法）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

import {
  MAX_TIERS,
  MIN_TIER_TIERS,
  POLICY_NAME_MAX,
  REBATE_RATE_UNITS,
  REBATE_RULE_TYPES,
  REBATE_SETTLE_PERIODS,
  addTier,
  buildPolicyPayload,
  emptyPolicyForm,
  emptyPolicyFilter,
  isPolicyFiltered,
  onRuleTypeChange,
  policyEmptyText,
  policyFormFrom,
  policyRuleSummary,
  policyStatusLabel,
  rateText,
  removeTier,
  ruleTypeLabel,
  syncTierBound,
  tierPreviewText,
  tierRangeText,
  toPlain,
  validityText,
  validatePolicyForm,
  validateRules,
  type PolicyFormDraft,
  type TierDraft,
} from '@/views/business/erp/rebate/policy-logic';

import {
  REBATE_SETTLEMENT_STATUSES,
  SETTLE_REMARK_MAX,
  baseCellText,
  baseDetailText,
  buildRunReq,
  buildSettleReq,
  emptySettlementFilter,
  invoiceText,
  isEditable,
  isNegativeBase,
  isSettlementFiltered,
  monthPeriodOptions,
  normalizePeriodCode,
  pageTotals,
  periodCodeLabel,
  periodRangeText,
  previewHeadline,
  previewTierLines,
  previewWarnings,
  quarterPeriodOptions,
  runItemText,
  runResultGroups,
  runResultText,
  settleStatusLabel,
  settleStatusTheme,
  settlementEmptyText,
  settledAtText,
  tierHitText,
  validatePeriodCode,
} from '@/views/business/erp/rebate/settle-logic';

// 注意：**不能**从这里 import 运行时值（`REBATE_PERMISSIONS`）—— `api/business/erp/rebate.ts`
// 顶层 import 了 `@/api/request`（读 `import.meta.env`），单测跑在 node 下会在加载阶段就崩
// （本仓 `pnpm test:unit` 的既有约束，见 `commission-logic.ts` 头注释）。权限码在下面用字面量
// 逐字断言迁移文件 —— 那才是真契约。
import type { RebatePolicyRow, RebatePreview, RebateRunRes, RebateSettlementRow } from '@/api/business/erp/rebate';

/** 与 `V0438` / `V0439` / `V0440` 的 auth_code 逐字一致（同时也是 `api/rebate.ts` 里那份常量） */
const PERM = {
  policyList: 'business:erp:rebate-policy:list',
  policyCreate: 'business:erp:rebate-policy:create',
  policyUpdate: 'business:erp:rebate-policy:update',
  policyDelete: 'business:erp:rebate-policy:delete',
  settleList: 'business:erp:rebate-settlement:list',
  settleCalc: 'business:erp:rebate-settlement:calc',
  settleDelete: 'business:erp:rebate-settlement:delete',
} as const;

const HERE = dirname(fileURLToPath(import.meta.url));
/** 仓库根（tests/unit → frontend → 仓库根） */
const REPO_ROOT = resolve(HERE, '..', '..', '..');

function readRepo(relative: string): string {
  return readFileSync(resolve(REPO_ROOT, relative), 'utf8');
}

/** 一档草稿（默认「从 0 起、无上限」） */
function tier(overrides: Partial<TierDraft> = {}): TierDraft {
  return { tier_from: '0', tier_to: '', rate: '0.5', remark: '', ...overrides };
}

/** 政策行桩（`rules` 按 seq 升序，与后端出参一致） */
function policyRow(overrides: Partial<RebatePolicyRow> = {}): RebatePolicyRow {
  return {
    id: 'b0000000-0000-4000-8000-000000000001',
    policy_no: 'BR-20261117-000001',
    policy_name: '2026 年度华东返利',
    customer_id: 'c0000000-0000-4000-8000-000000000001',
    customer_code: 'CUS-0001',
    customer_name: '华东经销商',
    rule_type: 'BY_QTY',
    rate_unit: 'PER_UNIT',
    settle_period: 'MONTH',
    effective_from: '2026-01-01',
    effective_to: null,
    status: 1,
    remark: null,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    rules: [
      { id: 'r1', seq: 1, tier_from: '0.0000', tier_to: null, rate: '0.500000', remark: null },
    ],
    ...overrides,
  };
}

/** 结算单桩 */
function settleRow(overrides: Partial<RebateSettlementRow> = {}): RebateSettlementRow {
  return {
    id: 'a0000000-0000-4000-8000-000000000001',
    settlement_no: 'RB-20261117-000001',
    customer_id: 'c0000000-0000-4000-8000-000000000001',
    customer_code: 'CUS-0001',
    customer_name: '华东经销商',
    settle_period: 'MONTH',
    settle_period_label: '月度',
    period_code: '2026-10',
    period_from: '2026-10-01',
    period_to: '2026-11-01',
    period_last_day: '2026-10-31',
    policy_id: 'b0000000-0000-4000-8000-000000000001',
    policy_no: 'BR-20261117-000001',
    policy_name: '2026 年度华东返利',
    rule_type: 'BY_QTY',
    rule_type_label: '按量',
    rate_unit: 'PER_UNIT',
    rate_unit_label: '元/件',
    policy_effective_from: '2026-01-01',
    policy_effective_to: null,
    policy_tiers: [{ seq: 1, tier_from: '0.0000', tier_to: null, rate: '0.500000', remark: null }],
    base: {
      order_count: 3,
      order_amount: '10000.0000',
      order_qty: '1000.0000',
      return_count: 1,
      return_amount: '1000.0000',
      return_qty: '100.0000',
      net_amount: '9000.0000',
      net_qty: '900.0000',
    },
    tier_seq: 1,
    tier_from: '0.0000',
    tier_to: null,
    rate: '0.500000',
    rebate_amount: '450.00',
    status: 'DRAFT',
    status_label: '草稿',
    remark: null,
    created_at: '2026-11-01T00:00:00Z',
    updated_at: '2026-11-01T00:00:00Z',
    invoice_id: null,
    invoice_no: null,
    settled_at: null,
    ...overrides,
  };
}

// ===========================================================================
// ① 源码即契约：域值 / 权限码 / 菜单 path
// ===========================================================================

test('域值与后端常量逐字一致（改一个字母就是「插不进库 / 授不上权」）', () => {
  const dto = readRepo('backend/app/src/business/erp/sales/rebate_dto.rs');

  assert.ok(
    dto.includes('pub const RULE_TYPES: [&str; 3] = [RULE_BY_QTY, RULE_BY_AMOUNT, RULE_TIERED];'),
    '后端必须是 3 种规则类型（BY_QTY / BY_AMOUNT / TIERED）',
  );
  assert.ok(dto.includes('pub const RATE_UNITS: [&str; 2] = [UNIT_PER_UNIT, UNIT_PERCENT];'));
  assert.ok(dto.includes('pub const SETTLE_PERIODS: [&str; 2] = [SETTLE_MONTH, SETTLE_QUARTER];'));
  assert.ok(dto.includes('pub const MAX_TIERS: usize = 20;'));
  assert.ok(dto.includes('pub const MIN_TIER_TIERS: usize = 2;'));
  assert.ok(dto.includes('pub const STATUS_ENABLED: i16 = 1;'));
  assert.ok(dto.includes('pub const STATUS_DISABLED: i16 = 0;'));

  // 前端镜像（顺序也一致：下拉顺序）
  assert.deepEqual([...REBATE_RULE_TYPES], ['BY_QTY', 'BY_AMOUNT', 'TIERED']);
  assert.deepEqual([...REBATE_RATE_UNITS], ['PER_UNIT', 'PERCENT']);
  assert.deepEqual([...REBATE_SETTLE_PERIODS], ['MONTH', 'QUARTER']);
  assert.equal(MAX_TIERS, 20);
  assert.equal(MIN_TIER_TIERS, 2);

  // 结算状态三值（DRAFT / SETTLED / CANCELLED）
  const settleDto = readRepo('backend/app/src/business/erp/sales/rebate_settle_dto.rs');
  assert.ok(settleDto.includes('pub const STATUSES: [&str; 3] = [STATUS_DRAFT, STATUS_SETTLED, STATUS_CANCELLED];'));
  assert.deepEqual([...REBATE_SETTLEMENT_STATUSES], ['DRAFT', 'SETTLED', 'CANCELLED']);
});

test('8 个权限码与迁移逐字一致（V0438 政策 4 / V0439 结算 3 / V0440 开票 1）', () => {
  const v0438 = readRepo('backend/app/migrations/business/V0438__sales_rebate_policy.up.sql');
  const v0439 = readRepo('backend/app/migrations/business/V0439__sales_rebate_settlement.up.sql');
  const v0440 = readRepo('backend/app/migrations/business/V0440__sales_rebate_invoice.up.sql');

  for (const code of [
    PERM.policyList,
    PERM.policyCreate,
    PERM.policyUpdate,
    PERM.policyDelete,
  ]) {
    assert.ok(v0438.includes(`'${code}'`), `V0438 里必须有 ${code}`);
  }
  for (const code of [
    PERM.settleList,
    PERM.settleCalc,
    PERM.settleDelete,
  ]) {
    assert.ok(v0439.includes(`'${code}'`), `V0439 里必须有 ${code}`);
  }
  assert.ok(v0440.includes("'business:erp:rebate-settlement:invoice'"), 'V0440 里必须开票码');

  // 前端用到的 7 个码互不相同，且 write 码不是 :list
  const used = Object.values(PERM);
  assert.equal(new Set(used).size, used.length, '权限码不能重复');
  for (const code of [
    PERM.policyCreate,
    PERM.policyUpdate,
    PERM.policyDelete,
    PERM.settleCalc,
    PERM.settleDelete,
  ]) {
    assert.ok(!code.endsWith(':list'), `${code} 是写接口的码，不能是读码`);
  }
});

test('菜单收尾迁移与前端路由逐字一致（V0443 的 path / component；n08 会整串比对）', () => {
  const v0443 = readRepo('backend/app/migrations/business/V0443__sales_rebate_menu_page.up.sql');
  const router = readRepo('frontend/src/router/index.ts');

  assert.ok(v0443.includes("v_path      text := '/business/erp/rebate-policy';"), 'V0443 必须把 path 补成 /business/erp/rebate-policy');
  assert.ok(v0443.includes("v_component text := 'business/erp/rebate/index';"));
  assert.ok(v0443.includes("v_title     text := '销售返利';"), 'title 是侧栏显示名，必须是短名');
  assert.ok(v0443.includes("'20000000-0000-0000-0000-000000004380'::uuid"), '补的是 V0438 的页面节点 …4380');
  assert.ok(v0443.includes('v_count <> 7'), '必须自检 7 个按钮仍挂在页面节点下（政策 3 + 结算 3 + 开票 1）');

  // 路由侧：path 去前缀后与 sys_menu.path 的同名段一致
  assert.ok(router.includes("path: 'business/erp/rebate-policy',"));
  assert.ok(router.includes("component: () => import('@/views/business/erp/rebate/index.vue'),"));
  assert.ok(router.includes("name: 'BusinessErpRebatePolicy',"));
  assert.ok(router.includes("permission: 'business:erp:rebate-policy:list'"));
  assert.ok(
    !router.includes("path: 'business/erp/rebate-settlement'"),
    '只允许一个页面节点（结算在第二个 tab）：菜单总数已顶 menu_invariants ⑤ 上限',
  );
});

// ===========================================================================
// ② 政策：三种规则类型的形状矩阵（与后端 checked_rules 同序）
// ===========================================================================

test('按量 / 按额：恰好 1 档且 0 ~ ∞（多档 / 起征 / 封顶全部拒）', () => {
  // 正确形状
  assert.equal(validateRules('BY_QTY', 'PER_UNIT', [tier()]), null);
  assert.equal(validateRules('BY_AMOUNT', 'PERCENT', [tier({ rate: '2' })]), null);

  // 档数必须 1
  assert.match(
    validateRules('BY_QTY', 'PER_UNIT', [tier(), tier({ tier_from: '100', tier_to: '' })]) ?? '',
    /只能有 1 档/,
  );

  // 必须从 0 起
  assert.match(validateRules('BY_QTY', 'PER_UNIT', [tier({ tier_from: '100' })]) ?? '', /必须从 0 起/);

  // 必须无上限
  assert.match(validateRules('BY_QTY', 'PER_UNIT', [tier({ tier_to: '1000' })]) ?? '', /上限留空/);
});

test('阶梯：2~20 档首尾相接、首档从 0、末档开口（五个反例逐一命中）', () => {
  const good: TierDraft[] = [
    tier({ tier_from: '0', tier_to: '1000' }),
    tier({ tier_from: '1000', tier_to: '5000' }),
    tier({ tier_from: '5000', tier_to: '', rate: '0.3' }),
  ];
  assert.equal(validateRules('TIERED', 'PER_UNIT', good), null, '首尾相接的三档合法');
  assert.equal(
    validateRules('TIERED', 'PERCENT', [
      tier({ tier_from: '0', tier_to: '10000', rate: '1' }),
      tier({ tier_from: '10000', tier_to: '', rate: '2' }),
    ]),
    null,
    '百分比阶梯合法（rate ≤ 100）',
  );

  // 只有 1 档
  assert.match(validateRules('TIERED', 'PER_UNIT', [tier()]) ?? '', /至少要 2 档/);

  // 首档不从 0 起
  assert.match(
    validateRules('TIERED', 'PER_UNIT', [
      tier({ tier_from: '10', tier_to: '100' }),
      tier({ tier_from: '100', tier_to: '' }),
    ]) ?? '',
    /必须从 0 起/,
  );

  // 中间断档
  assert.match(
    validateRules('TIERED', 'PER_UNIT', [
      tier({ tier_from: '0', tier_to: '1000' }),
      tier({ tier_from: '2000', tier_to: '' }),
    ]) ?? '',
    /接不上/,
  );

  // 末档封顶
  assert.match(
    validateRules('TIERED', 'PER_UNIT', [
      tier({ tier_from: '0', tier_to: '1000' }),
      tier({ tier_from: '1000', tier_to: '5000' }),
    ]) ?? '',
    /最后一档必须留空上限/,
  );

  // 下限重复
  assert.match(
    validateRules('TIERED', 'PER_UNIT', [
      tier({ tier_from: '0', tier_to: '1000' }),
      tier({ tier_from: '0', tier_to: '' }),
    ]) ?? '',
    /下限重复|区间不成立/,
  );

  // 超过 20 档
  const tooMany: TierDraft[] = Array.from({ length: MAX_TIERS + 1 }, (_, index) =>
    tier({
      tier_from: index === 0 ? '0' : String(index * 100),
      tier_to: index === MAX_TIERS ? '' : String((index + 1) * 100),
    }),
  );
  assert.match(validateRules('TIERED', 'PER_UNIT', tooMany) ?? '', /最多 20 档/);
});

test('比率与档位数值：> 0、百分比 ≤ 100、小数位上限（库层 NUMERIC 口径）', () => {
  // 0 元档不允许（会让人以为「这一档不返」）
  assert.match(validateRules('BY_QTY', 'PER_UNIT', [tier({ rate: '0' })]) ?? '', /必须大于 0/);
  // 负数 / 空 / 非法
  assert.match(validateRules('BY_QTY', 'PER_UNIT', [tier({ rate: '-1' })]) ?? '', /必须大于 0/);
  assert.match(validateRules('BY_QTY', 'PER_UNIT', [tier({ rate: '' })]) ?? '', /缺少比率/);
  // 百分比 100 合法、100.0001 拒
  assert.equal(validateRules('BY_AMOUNT', 'PERCENT', [tier({ rate: '100' })]), null);
  assert.match(validateRules('BY_AMOUNT', 'PERCENT', [tier({ rate: '100.0001' })]) ?? '', /不能超过 100/);
  // 元/件可以远大于 100（这是单价不是百分数）
  assert.equal(validateRules('BY_QTY', 'PER_UNIT', [tier({ rate: '500' })]), null);
  // 比率最多 6 位小数；档位值最多 4 位
  assert.match(validateRules('BY_QTY', 'PER_UNIT', [tier({ rate: '0.1234567' })]) ?? '', /最多 6 位小数/);
  assert.match(
    validateRules('TIERED', 'PER_UNIT', [
      tier({ tier_from: '0', tier_to: '1000.12345' }),
      tier({ tier_from: '1000.12345', tier_to: '' }),
    ]) ?? '',
    /最多 4 位小数/,
  );
  // 上下限相等 = 空区间
  assert.match(
    validateRules('TIERED', 'PER_UNIT', [
      tier({ tier_from: '0', tier_to: '100' }),
      tier({ tier_from: '100', tier_to: '' }),
      tier({ tier_from: '100', tier_to: '100' }),
    ]) ?? '',
    /区间不成立|下限重复/,
  );
});

test('表单整体校验：名称 / 客户 / 粒度 / 生效日 / 有效期（按「先必填再语义」）', () => {
  const base: PolicyFormDraft = { ...emptyPolicyForm('2026-11-17'), policy_name: '返利', customer_id: 'c-1', rules: [tier()] };
  assert.equal(validatePolicyForm(base), null);

  assert.match(validatePolicyForm({ ...base, policy_name: '  ' }) ?? '', /请填写政策名称/);
  assert.match(
    validatePolicyForm({ ...base, policy_name: 'x'.repeat(POLICY_NAME_MAX + 1) }) ?? '',
    /最多 200 字/,
  );
  assert.match(validatePolicyForm({ ...base, customer_id: '' }) ?? '', /请选择客户/);
  assert.match(
    validatePolicyForm({ ...base, effective_from: '2026/11/17' }) ?? '',
    /请选择生效日/,
  );
  assert.match(
    validatePolicyForm({ ...base, effective_from: '2026-11-17', effective_to: '2026-11-01' }) ?? '',
    /不能早于/,
  );
  assert.equal(
    validatePolicyForm({ ...base, effective_from: '2026-11-17', effective_to: '2026-11-17' }),
    null,
    '同一天合法（两端都含）',
  );
  // 阶梯必须显式计价单位：把 rate_unit 弄成非法值必须被拦
  assert.match(
    validatePolicyForm({
      ...base,
      rule_type: 'TIERED',
      rate_unit: 'BOGUS' as never,
      rules: [tier({ tier_from: '0', tier_to: '100' }), tier({ tier_from: '100', tier_to: '' })],
    }) ?? '',
    /必须选计价单位/,
  );
});

// ===========================================================================
// ③ 政策：表单整形与入参
// ===========================================================================

test('切换规则类型：按量/按额压成单档并钉死单位，阶梯补两档模板', () => {
  const form: PolicyFormDraft = { ...emptyPolicyForm('2026-11-17'), rules: [tier({ rate: '2' })] };

  onRuleTypeChange(form, 'BY_AMOUNT');
  assert.equal(form.rule_type, 'BY_AMOUNT');
  assert.equal(form.rate_unit, 'PERCENT', '按额必须是销售额百分比');
  assert.equal(form.rules.length, 1, '按额是平铺单档');
  assert.equal(form.rules[0].rate, '2', '已填的费率要保留，别让人重填');

  onRuleTypeChange(form, 'TIERED');
  assert.equal(form.rule_type, 'TIERED');
  assert.equal(form.rules.length, MIN_TIER_TIERS, '阶梯至少两档 → 切换时补模板');
  assert.equal(form.rules[0].tier_from, '0', '模板首档从 0 起');
  assert.equal(form.rules[1].tier_to, '', '模板末档开口');
  assert.equal(Number(form.rules[0].tier_to), Number(form.rules[1].tier_from), '模板首尾相接');

  onRuleTypeChange(form, 'BY_QTY');
  assert.equal(form.rate_unit, 'PER_UNIT');
  assert.equal(form.rules.length, 1, '回到按量必须压回单档（否则后端 66089）');
});

test('加档 / 删档：只有末档可开口，删只删最后一档', () => {
  const form: PolicyFormDraft = { ...emptyPolicyForm('2026-11-17') };
  onRuleTypeChange(form, 'TIERED');
  // 模板：0~1000 + 1000~∞
  const added = addTier(form);
  assert.equal(added.ok, false, '末档还开着，先填上限才能加档');
  assert.match(added.reason ?? '', /先给当前最后一档填上限/);

  // 填上末档上限后可以加档，且新档的下限 = 上一档的上限
  form.rules[form.rules.length - 1].tier_to = '5000';
  const ok = addTier(form);
  assert.equal(ok.ok, true);
  assert.equal(form.rules.length, 3);
  assert.equal(form.rules[2].tier_from, '5000');
  assert.equal(form.rules[2].tier_to, '', '新档必须开口');

  // 只能删最后一档
  assert.equal(removeTier(form, 0).ok, false);
  assert.equal(removeTier(form, 2).ok, true);
  assert.equal(form.rules.length, 2);
  assert.equal(form.rules[1].tier_to, '', '删完后新末档必须开口');

  // 已是 2 档时不能再删
  assert.equal(removeTier(form, 1).ok, false);
  assert.match(removeTier(form, 1).reason ?? '', /至少要 2 档/);

  // 按量/按额不能加档
  const flat: PolicyFormDraft = { ...emptyPolicyForm('2026-11-17') };
  assert.equal(addTier(flat).ok, false);
});

test('改一档的上限 → 下一档下限跟着走（保持首尾相接）', () => {
  const form: PolicyFormDraft = { ...emptyPolicyForm('2026-11-17') };
  onRuleTypeChange(form, 'TIERED');
  form.rules[0].tier_to = '2500';
  syncTierBound(form, 0);
  assert.equal(form.rules[1].tier_from, '2500');
  // 末档没有下一档：不报错、不改任何东西
  syncTierBound(form, 1);
  assert.equal(form.rules[1].tier_from, '2500');
});

test('入参：数值转字符串、空上限发 null、rate_unit 显式发、status 是数字', () => {
  const form: PolicyFormDraft = {
    ...emptyPolicyForm('2026-11-17'),
    policy_name: '  华东返利  ',
    customer_id: 'c-1',
    rule_type: 'TIERED',
    rate_unit: 'PERCENT',
    settle_period: 'QUARTER',
    effective_from: '2026-01-01',
    effective_to: '2026-12-31',
    status: 0,
    remark: '   ',
    rules: [
      { tier_from: '0', tier_to: '100000', rate: '1', remark: '  基础档  ' },
      { tier_from: '100000', tier_to: '', rate: '2', remark: '' },
    ],
  };
  const payload = buildPolicyPayload(form);
  assert.deepEqual(payload, {
    policy_name: '华东返利',
    customer_id: 'c-1',
    rule_type: 'TIERED',
    rate_unit: 'PERCENT',
    settle_period: 'QUARTER',
    effective_from: '2026-01-01',
    effective_to: '2026-12-31',
    status: 0,
    remark: null,
    rules: [
      { tier_from: '0', tier_to: '100000', rate: '1', remark: '基础档' },
      { tier_from: '100000', tier_to: null, rate: '2', remark: null },
    ],
  });

  // 后端 NUMERIC 收字符串：小数位不丢、不带浮点尾巴
  const num = buildPolicyPayload({
    ...form,
    rule_type: 'BY_QTY',
    rate_unit: 'PER_UNIT',
    rules: [{ tier_from: '0.0000', tier_to: '', rate: '0.1', remark: '' }],
  });
  assert.equal(num.rules[0].tier_from, '0');
  assert.equal(num.rules[0].rate, '0.1');
  assert.equal(num.rules[0].tier_to, null);
});

test('编辑回填：全量档位、null 折空串、按 tier_from 升序（整单覆盖不能丢档）', () => {
  const row = policyRow({
    rule_type: 'TIERED',
    rate_unit: 'PERCENT',
    rules: [
      { id: 'r2', seq: 2, tier_from: '100000.0000', tier_to: null, rate: '2.000000', remark: '高档' },
      { id: 'r1', seq: 1, tier_from: '0.0000', tier_to: '100000.0000', rate: '1.000000', remark: null },
    ],
  });
  const form = policyFormFrom(row);
  assert.equal(form.rule_type, 'TIERED');
  assert.equal(form.customer_display, 'CUS-0001 华东经销商');
  assert.equal(form.rules.length, 2, '两个档位都要带回弹窗');
  assert.deepEqual(
    form.rules.map((item) => item.tier_from),
    ['0', '100000'],
    '按 tier_from 升序（后端也是这么编号的）',
  );
  assert.equal(form.rules[0].tier_to, '100000', '`0.0000` / `100000.0000` 折成 `0` / `100000`');
  assert.equal(form.rules[1].tier_to, '', 'null 上限折成空串');
  assert.equal(toPlain('0.500000'), '0.5');
  assert.equal(toPlain(null), '');
});

// ===========================================================================
// ④ 政策：展示文案
// ===========================================================================

test('展示：类型中文 / 费率单位 / 档位摘要 / 有效期 / 状态', () => {
  assert.equal(ruleTypeLabel('BY_QTY'), '按量');
  assert.equal(ruleTypeLabel('WEIRD'), 'WEIRD', '未知值回落编码，不显示空白');
  assert.equal(rateText('0.500000', 'PER_UNIT'), '0.5 元/件');
  assert.equal(rateText('2.000000', 'PERCENT'), '2%');
  assert.equal(tierRangeText('0', null), '0 ~ +∞');
  assert.equal(tierRangeText('1000.0000', '5000.0000'), '1000 ~ 5000');

  assert.match(policyRuleSummary(policyRow()), /平铺 0.5 元\/件/);
  const tiered = policyRuleSummary(
    policyRow({
      rule_type: 'TIERED',
      rate_unit: 'PERCENT',
      rules: [
        { id: 'r1', seq: 1, tier_from: '0', tier_to: '1000', rate: '1', remark: null },
        { id: 'r2', seq: 2, tier_from: '1000', tier_to: '5000', rate: '1.5', remark: null },
        { id: 'r3', seq: 3, tier_from: '5000', tier_to: null, rate: '2', remark: null },
      ],
    }),
  );
  assert.match(tiered, /^3 档：0 ~ 1000 \/ 1000 ~ 5000 \/ 5000 ~ \+∞$/);

  assert.equal(validityText('2026-01-01', null), '2026-01-01 ~ 长期');
  assert.equal(validityText('2026-01-01', '2026-12-31'), '2026-01-01 ~ 2026-12-31');
  assert.equal(policyStatusLabel(1), '启用');
  assert.equal(policyStatusLabel(0), '停用');

  // 档位速览（弹窗里的可读回显）
  const form: PolicyFormDraft = { ...emptyPolicyForm('2026-11-17') };
  onRuleTypeChange(form, 'TIERED');
  const lines = tierPreviewText(form);
  assert.equal(lines.length, 2);
  assert.equal(lines[0], '第 1 档 0 ~ 1000 ⇒ （未填费率）');
  assert.equal(lines[1], '第 2 档 1000 ~ +∞ ⇒ （未填费率）');
});

test('筛选与空态：带筛选条件时才说「没有匹配」', () => {
  const empty = emptyPolicyFilter();
  assert.equal(isPolicyFiltered(empty), false);
  assert.match(policyEmptyText(empty), /还没有返利政策/);

  assert.equal(isPolicyFiltered({ ...empty, status: 0 }), true, '`status=0`（停用）也算带了筛选');
  assert.match(policyEmptyText({ ...empty, rule_type: 'TIERED' }), /没有匹配/);
});

// ===========================================================================
// ⑤ 结算：期间码（粒度与形状互相钉死）
// ===========================================================================

test('期间码：月度配 yyyy-MM、季度配 yyyy-QN，串配一律拒（与 PeriodArgs::resolve 同口径）', () => {
  assert.equal(validatePeriodCode('MONTH', '2026-11'), null);
  assert.equal(validatePeriodCode('MONTH', ' 2026-01 '), null, '首尾空白要容忍');
  assert.match(validatePeriodCode('MONTH', '2026-13') ?? '', /yyyy-MM/);
  assert.match(validatePeriodCode('MONTH', '2026-Q1') ?? '', /yyyy-MM/, '月度不能配季度期间码');
  assert.match(validatePeriodCode('MONTH', '2026-1') ?? '', /yyyy-MM/);
  assert.match(validatePeriodCode('MONTH', '') ?? '', /请选择结算期间/);

  assert.equal(validatePeriodCode('QUARTER', '2026-Q1'), null);
  assert.equal(validatePeriodCode('QUARTER', '2026-q4'), null, '小写季号归一化后合法');
  assert.match(validatePeriodCode('QUARTER', '2026-Q5') ?? '', /yyyy-QN/);
  assert.match(validatePeriodCode('QUARTER', '2026-11') ?? '', /yyyy-QN/, '季度不能配月度期间码');

  assert.equal(normalizePeriodCode(' 2026-q2 '), '2026-Q2');
  assert.equal(normalizePeriodCode(''), '');
  assert.equal(periodCodeLabel('2026-11'), '2026 年 11 月');
  assert.equal(periodCodeLabel('2026-Q3'), '2026 年第 3 季度');
  assert.equal(periodCodeLabel(''), '—');
  assert.equal(periodRangeText('2026-10-01', '2026-10-31'), '2026-10-01 ~ 2026-10-31');
});

test('期间候选：新的排最前、不含未来月、季度从本季起', () => {
  const now = new Date(2026, 10, 17); // 2026-11-17（本地时区）
  const months = monthPeriodOptions(now);
  assert.equal(months.length, 13);
  assert.equal(months[0].value, '2026-11');
  assert.equal(months[1].value, '2026-10');
  assert.equal(months[12].value, '2025-11', '往前整 12 个月');
  assert.equal(months[0].label, '2026 年 11 月');

  const quarters = quarterPeriodOptions(now);
  assert.equal(quarters.length, 8);
  assert.equal(quarters[0].value, '2026-Q4', '11 月属于第 4 季度');
  assert.equal(quarters[1].value, '2026-Q3');
  assert.equal(quarters[4].value, '2025-Q4');
});

// ===========================================================================
// ⑥ 结算：入参 / 展示 / 结果文案
// ===========================================================================

test('入参：期间码归一化、空备注不发、跑批空客户列表不发 customer_ids', () => {
  assert.deepEqual(buildSettleReq(' c-1 ', 'MONTH', ' 2026-10 ', '  '), {
    customer_id: 'c-1',
    settle_period: 'MONTH',
    period_code: '2026-10',
  });
  assert.deepEqual(buildSettleReq('c-1', 'QUARTER', '2026-q1', '手工补算'), {
    customer_id: 'c-1',
    settle_period: 'QUARTER',
    period_code: '2026-Q1',
    remark: '手工补算',
  });

  assert.deepEqual(buildRunReq('MONTH', '2026-10'), {
    settle_period: 'MONTH',
    period_code: '2026-10',
  });
  assert.deepEqual(buildRunReq('MONTH', '2026-10', [' c-1 ', '', 'c-2'], '跑批'), {
    settle_period: 'MONTH',
    period_code: '2026-10',
    customer_ids: ['c-1', 'c-2'],
    remark: '跑批',
  });
});

test('展示：基数（负数为退货多于销售）/ 命中档 / 发票 / 状态 / 转结时间', () => {
  const row = settleRow();
  assert.equal(baseCellText(row.base), '9,000.00 / 900');
  assert.match(baseDetailText(row.base), /订单 3 张 \/ 10,000\.00、1000 件 − 退货 1 张 \/ 1,000\.00、100 件 = 净额 9,000\.00、净量 900/);
  assert.equal(isNegativeBase(row.base), false);
  assert.equal(
    isNegativeBase({ ...row.base, net_amount: '-500.0000', net_qty: '-10.0000' }),
    true,
    '退货多于销售 → 净基数为负（按 0 落档）',
  );

  assert.equal(tierHitText(row), '第 1 档 0 ~ +∞ ⇒ 0.5 元/件');
  assert.equal(
    tierHitText({ ...row, tier_seq: 2, tier_from: '1000', tier_to: '5000', rate: '2', rate_unit: 'PERCENT' }),
    '第 2 档 1000 ~ 5000 ⇒ 2%',
  );
  assert.equal(tierHitText({ ...row, tier_seq: null }), '未命中档位');

  assert.equal(invoiceText(row), '未开票（草稿）');
  assert.equal(invoiceText({ ...row, status: 'SETTLED', invoice_no: 'INV-202611-0009' }), 'INV-202611-0009');
  assert.equal(invoiceText({ ...row, status: 'SETTLED' }), '—');

  assert.equal(settleStatusLabel('DRAFT'), '草稿');
  assert.equal(settleStatusLabel('SETTLED'), '已结算');
  assert.equal(settleStatusTheme('CANCELLED'), 'danger');
  assert.equal(isEditable(row), true);
  assert.equal(isEditable({ ...row, status: 'SETTLED' }), false, '已结算只能红冲，不能重算/作废');
  assert.equal(settledAtText(null), '—');
  assert.equal(settledAtText('2026-11-17T03:04:05Z'), '2026-11-17');
});

test('本页合计只汇总当前页（不是全量 —— 页面上也这么写）', () => {
  const totals = pageTotals([
    settleRow(),
    settleRow({ id: 'a0000000-0000-4000-8000-000000000002', settlement_no: 'RB-20261117-000002' }),
  ]);
  assert.equal(totals.count, 2);
  assert.equal(totals.netAmount, 18000);
  assert.equal(totals.rebateAmount, 900);
});

test('试算结论：无政策时不能说「应返 0 元」，要说「没有政策可算」', () => {
  const preview: RebatePreview = {
    customer_id: 'c-1',
    customer_code: 'CUS-0001',
    customer_name: '华东经销商',
    settle_period: 'MONTH',
    settle_period_label: '月度',
    period_code: '2026-10',
    period_from: '2026-10-01',
    period_to: '2026-11-01',
    period_last_day: '2026-10-31',
    policy: null,
    base: settleRow().base,
    tier_seq: null,
    tier_from: '0',
    tier_to: null,
    rate: '0',
    rebate_amount: '0.00',
    warnings: ['该期间没有覆盖整期的启用政策', '  '],
    existing: null,
  };
  assert.match(previewHeadline(preview), /没有覆盖整期的启用政策/);
  assert.match(previewHeadline(preview), /2026 年 10 月/);
  assert.deepEqual(previewWarnings(preview), ['该期间没有覆盖整期的启用政策'], '空白提示要过滤掉');
  assert.deepEqual(previewTierLines(preview), []);

  const withPolicy: RebatePreview = {
    ...preview,
    policy: {
      policy_id: 'b1',
      policy_no: 'BR-20261117-000001',
      policy_name: '华东返利',
      rule_type: 'TIERED',
      rate_unit: 'PERCENT',
      effective_from: '2026-01-01',
      effective_to: null,
      tiers: [
        { seq: 1, tier_from: '0', tier_to: '10000', rate: '1', remark: null },
        { seq: 2, tier_from: '10000', tier_to: null, rate: '2', remark: null },
      ],
    },
    tier_seq: 2,
    tier_from: '10000',
    tier_to: null,
    rate: '2',
    rebate_amount: '180.00',
  };
  assert.match(previewHeadline(withPolicy), /应返 180\.00/);
  assert.match(previewHeadline(withPolicy), /BR-20261117-000001/);
  assert.deepEqual(previewTierLines(withPolicy), [
    '第 1 档 0 ~ 10000 ⇒ 1%',
    '第 2 档 10000 ~ +∞ ⇒ 2%',
  ]);
});

test('跑批结果：成功 / 跳过 / 失败都要说，失败排最前', () => {
  const res: RebateRunRes = {
    settle_period: 'MONTH',
    settle_period_label: '月度',
    period_code: '2026-10',
    period_from: '2026-10-01',
    period_to: '2026-11-01',
    total: 3,
    created: 1,
    skipped: 1,
    failed: 1,
    items: [
      {
        customer_id: 'c-1',
        customer_code: 'CUS-0001',
        customer_name: '华东',
        outcome: 'CREATED',
        settlement_id: 'a1',
        settlement_no: 'RB-20261117-000001',
        status: 'DRAFT',
        rebate_amount: '450.00',
        message: null,
      },
      {
        customer_id: 'c-2',
        customer_code: 'CUS-0002',
        customer_name: '华南',
        outcome: 'EXISTS',
        settlement_id: 'a2',
        settlement_no: 'RB-20261117-000002',
        status: 'DRAFT',
        rebate_amount: '10.00',
        message: null,
      },
      {
        customer_id: 'c-3',
        customer_code: null,
        customer_name: null,
        outcome: 'FAILED',
        settlement_id: null,
        settlement_no: null,
        status: null,
        rebate_amount: null,
        message: '客户已冻结',
      },
    ],
  };
  const summary = runResultText(res);
  assert.match(summary, /2026 年 10 月/);
  assert.match(summary, /共 3 家客户/);
  assert.match(summary, /新建 1 张/);
  assert.match(summary, /已有跳过 1 张/);
  assert.match(summary, /失败 1 家/);
  assert.ok(!summary.includes('失败 0'), '没有失败时不写「失败 0 家」');

  const groups = runResultGroups(res);
  assert.equal(groups.failed.length, 1);
  assert.equal(groups.created.length, 1);
  assert.equal(groups.skipped.length, 1);
  assert.match(runItemText(groups.created[0]), /新建 RB-20261117-000001，应返 450\.00/);
  assert.match(runItemText(groups.skipped[0]), /已有结算单 RB-20261117-000002（DRAFT），跳过/);
  assert.match(runItemText(groups.failed[0]), /失败 —— 客户已冻结/);
});

test('结算筛选与空态文案 + 备注上限与后端一致', () => {
  const empty = emptySettlementFilter();
  assert.equal(isSettlementFiltered(empty), false);
  assert.match(settlementEmptyText(empty), /还没有结算单/);
  assert.match(settlementEmptyText({ ...empty, period_code: '2026-10' }), /没有匹配/);

  // 备注上限逐字对齐后端常量（超长前端先拦，别等 400）
  const dto = readRepo('backend/app/src/business/erp/sales/rebate_settle_dto.rs');
  assert.ok(dto.includes('pub const MAX_REMARK_LEN: usize = 500;'));
  assert.equal(SETTLE_REMARK_MAX, 500);
});
