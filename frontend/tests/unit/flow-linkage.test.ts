/**
 * 联动规则配置页纯逻辑单测（卡 t_d0daa4f0 / P1-FIX-5A）。
 *
 * 运行：`pnpm exec tsx --test tests/unit/flow-linkage.test.ts`
 * 全量：`pnpm test:unit`
 *
 * # 为什么测 `linkage-logic.ts` 而不测 `.vue`
 *
 * 卡面两条硬要求都是纯判定：
 * - 「动作是白名单选择（不是自由输入）」→ `actionOptions` 必须把未接管动作置灰；
 * - 「不许配 SQL」→ `buildParamPayload` 产出的键集合**只能**等于动作声明的键集合，
 *   前端表单里根本没有「加一个键」的入口。
 * 断言口径与后端 `linkage/spec.rs` 逐字对齐：
 * `ParamKind::Int { min, max }` 闭区间 / 必填缺省即错 / 可选空值不写进 payload。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { LinkageAction, LinkageParam, LinkageRule, LinkageTrigger } from '../../src/api/business/flow/linkage';
import {
  actionOptions,
  actionWhitelistNote,
  buildParamPayload,
  draftAlertTheme,
  emptyForm,
  filterRules,
  formatParams,
  implementedActionCount,
  isHealthy,
  paramsToForm,
  phaseLabel,
  previewHighlights,
  ruleTriggerStatusText,
  severityLabel,
  statusLabelOf,
  statusOptions,
  triggerOptions,
} from '../../src/views/business/flow/linkage/linkage-logic';

/** 构造一个动作声明（默认已接管；`params` 省略 = 无参数） */
function action(
  overrides: Partial<LinkageAction> & { code: string; params?: LinkageParam[] },
): LinkageAction {
  return {
    label: overrides.code,
    phase: 'IN_TX',
    severity: 'CRITICAL',
    implemented: true,
    reuse: 'some/impl.rs::fn',
    params: [],
    ...overrides,
  };
}

/** 构造一条规则 */
function rule(overrides: Partial<LinkageRule>): LinkageRule {
  return {
    id: 'r-1',
    name: '规则',
    trigger_entity: 'STOCK_IN',
    trigger_entity_label: '收货入库单',
    trigger_status: 'POSTED',
    trigger_status_label: '已过账',
    action_code: 'SEND_NOTICE',
    action_label: '发通知',
    phase: 'AFTER_COMMIT',
    severity: 'NON_CRITICAL',
    implemented: true,
    action_params: {},
    enabled: true,
    seq: 0,
    created_at: '2026-11-05T10:00:00+00:00',
    updated_at: '2026-11-05T10:00:00+00:00',
    problems: [],
    ...overrides,
  };
}

const TRIGGERS: LinkageTrigger[] = [
  {
    entity: 'STOCK_IN',
    label: '收货入库单',
    statuses: ['DRAFT', 'PENDING_POST', 'POSTED'],
    // 与 statuses 逐项对齐（后端 TriggerRes.status_labels 的不变量，P1-FIX-12A）
    status_labels: [
      { code: 'DRAFT', label: '草稿' },
      { code: 'PENDING_POST', label: '待过账' },
      { code: 'POSTED', label: '已过账' },
    ],
    wired: true,
    wiring: 'stock_in.rs::post',
  },
  {
    entity: 'TASK',
    label: '任务',
    statuses: ['TODO', 'DOING'],
    // 这条模拟「后端没下发中文名」的老响应：下拉必须原样显示编码，不许编造
    status_labels: [],
    wired: false,
    wiring: '尚未接线',
  },
];

test('动作下拉：未接管动作置灰，文案带事务归属', () => {
  const options = actionOptions([
    action({ code: 'SEND_NOTICE', phase: 'AFTER_COMMIT' }),
    action({ code: 'LOCK_DOCUMENT', implemented: false }),
  ]);
  assert.equal(options.length, 2);
  assert.equal(options[0].disabled, false, '已接管动作可选');
  assert.match(options[0].label, /提交后执行/);
  assert.equal(options[1].disabled, true, '未接管动作必须置灰（后端会 400）');
  assert.match(options[1].label, /未接管/);
});

test('触发点 / 状态下拉：状态集来自后端下发，前端不自己维护', () => {
  assert.deepEqual(
    triggerOptions(TRIGGERS).map((item) => item.value),
    ['STOCK_IN', 'TASK'],
  );
  assert.deepEqual(
    statusOptions(TRIGGERS, 'STOCK_IN').map((item) => item.value),
    ['DRAFT', 'PENDING_POST', 'POSTED'],
  );
  assert.deepEqual(statusOptions(TRIGGERS, 'NOT_A_DOC'), [], '未知实体不编造状态');
});

test('状态下拉文案：中文（编码），value 仍是编码（P1-FIX-12B）', () => {
  const options = statusOptions(TRIGGERS, 'STOCK_IN');
  assert.deepEqual(
    options.map((item) => [item.label, item.value]),
    [
      ['草稿（DRAFT）', 'DRAFT'],
      ['待过账（PENDING_POST）', 'PENDING_POST'],
      ['已过账（POSTED）', 'POSTED'],
    ],
    '中文给人看、括号里的编码给排障对日志',
  );
  // 后端没下发 status_labels（老响应 / 字典缺项）：原样显示编码，不许编造中文
  assert.deepEqual(statusOptions(TRIGGERS, 'TASK'), [
    { label: 'TODO', value: 'TODO' },
    { label: 'DOING', value: 'DOING' },
  ]);
});

test('状态中文名查表：查不到回落编码（不显示空白）', () => {
  assert.equal(statusLabelOf(TRIGGERS, 'STOCK_IN', 'POSTED'), '已过账');
  assert.equal(statusLabelOf(TRIGGERS, 'STOCK_IN', 'GHOST'), 'GHOST', '脏状态回落编码');
  assert.equal(statusLabelOf(TRIGGERS, 'NOT_A_DOC', 'POSTED'), 'POSTED', '未知实体回落编码');
});

test('列表状态列：优先用后端行级中文名，缺字段回落编码', () => {
  assert.equal(ruleTriggerStatusText(rule({})), '已过账');
  assert.equal(ruleTriggerStatusText(rule({ trigger_status_label: '' })), 'POSTED');
});

test('动作白名单说明：X 只数已实现动作，且明说不能自行新增（P1-FIX-12B）', () => {
  const actions = [
    action({ code: 'SEND_NOTICE' }),
    action({ code: 'GENERATE_VOUCHER' }),
    action({ code: 'LOCK_DOCUMENT', implemented: false }),
  ];
  assert.equal(implementedActionCount(actions), 2, '未接管的占位动作不算「已实现」');
  const note = actionWhitelistNote(actions);
  assert.match(note, /当前 2 个已实现/);
  assert.match(note, /不能自行新增/);
  assert.match(note, /需开发/);
  assert.match(note, /参数可自由配置/);
  // 空字典（加载失败 / 首屏）也不能出现 undefined 或崩掉
  assert.match(actionWhitelistNote([]), /当前 0 个已实现/);
});

test('参数拼装：必填空值报错，可选空值不写进 payload', () => {
  const spec = action({
    code: 'SEND_NOTICE',
    params: [
      { key: 'mode', kind: { kind: 'enum', values: ['SHADOW', 'TAKEOVER'] }, required: false, note: '上线模式' },
      { key: 'title', kind: { kind: 'text', max_len: 10 }, required: false, note: '标题' },
    ],
  });
  const empty = buildParamPayload(spec, emptyForm(spec));
  assert.deepEqual(empty.errors, [], '全部可选 → 不报错');
  assert.deepEqual(empty.payload, {}, '空值不落键（写 "" 会触发布类型校验失败）');

  const required = action({
    code: 'GENERATE_VOUCHER',
    params: [{ key: 'biz_type', kind: { kind: 'text', max_len: 32 }, required: true, note: '业务类型' }],
  });
  const missing = buildParamPayload(required, {});
  assert.equal(missing.errors.length, 1);
  assert.match(missing.errors[0], /必填/);
});

test('参数拼装：键集合只能来自动作声明（禁配 SQL 的结构性保证）', () => {
  const spec = action({
    code: 'SEND_NOTICE',
    params: [{ key: 'mode', kind: { kind: 'enum', values: ['SHADOW', 'TAKEOVER'] }, required: false, note: '' }],
  });
  // 表单里塞了不在声明里的键（模拟有人手改前端状态）→ 不会被带进 payload
  const { payload, errors } = buildParamPayload(spec, { mode: 'TAKEOVER', sql: 'delete from biz_voucher' });
  assert.deepEqual(errors, []);
  assert.deepEqual(Object.keys(payload), ['mode'], '声明外的键（含 sql）一个都不许出现');
  assert.equal(payload.mode, 'TAKEOVER');
});

test('参数拼装：整数 / 区间 / 十进制 / 枚举 / UUID 列表', () => {
  const spec = action({
    code: 'CREATE_TASK',
    phase: 'AFTER_COMMIT',
    severity: 'NON_CRITICAL',
    params: [
      { key: 'due_days', kind: { kind: 'int', min: 0, max: 365 }, required: false, note: '到期天数' },
      { key: 'amount', kind: { kind: 'decimal' }, required: false, note: '金额' },
      { key: 'direction', kind: { kind: 'enum', values: ['IN', 'OUT'] }, required: true, note: '方向' },
      { key: 'ids', kind: { kind: 'uuid_list', max_items: 2 }, required: false, note: '明细' },
    ],
  });
  const ok = buildParamPayload(spec, { due_days: '30', amount: '12.5', direction: 'IN', ids: 'a, b' });
  assert.deepEqual(ok.errors, []);
  assert.equal(ok.payload.due_days, 30);
  assert.equal(ok.payload.amount, 12.5);
  assert.deepEqual(ok.payload.ids, ['a', 'b']);

  const tooBig = buildParamPayload(spec, { due_days: '366', direction: 'IN' });
  assert.equal(tooBig.errors.length, 1);
  assert.match(tooBig.errors[0], /不能大于 365/);

  const notInt = buildParamPayload(spec, { due_days: '1.5', direction: 'IN' });
  assert.match(notInt.errors[0], /必须是整数/);

  const badEnum = buildParamPayload(spec, { direction: 'SIDEWAYS' });
  assert.match(badEnum.errors[0], /必须是 IN \/ OUT 之一/);

  const tooMany = buildParamPayload(spec, { direction: 'IN', ids: 'a,b,c' });
  assert.match(tooMany.errors[0], /最多 2 项/);
});

test('表单回填：布尔 → boolean，UUID 列表 → 逗号串，声明外的键忽略', () => {
  const spec = action({
    code: 'SEND_NOTICE',
    params: [
      { key: 'force', kind: { kind: 'bool' }, required: false, note: '' },
      { key: 'ids', kind: { kind: 'uuid_list', max_items: 5 }, required: false, note: '' },
      { key: 'mode', kind: { kind: 'enum', values: ['SHADOW', 'TAKEOVER'] }, required: false, note: '' },
    ],
  });
  const form = paramsToForm(spec, { force: true, ids: ['u-1', 'u-2'], mode: 'SHADOW', sql: 'x' });
  assert.equal(form.force, true);
  assert.equal(form.ids, 'u-1, u-2');
  assert.equal(form.mode, 'SHADOW');
  assert.equal('sql' in form, false, '历史脏键不带进表单');
});

test('列表筛选：实体 / 只看启用 / 关键词（大小写不敏感）', () => {
  const rows = [
    rule({ id: 'a', trigger_entity: 'STOCK_IN', action_code: 'SEND_NOTICE', enabled: true }),
    rule({ id: 'b', trigger_entity: 'STOCK_IN', action_code: 'GENERATE_VOUCHER', enabled: false }),
    rule({ id: 'c', trigger_entity: 'TASK', action_code: 'SEND_NOTICE', enabled: true, name: '任务完成通知' }),
  ];
  assert.deepEqual(
    filterRules(rows, { entity: '', keyword: '', onlyEnabled: false }).map((item) => item.id),
    ['a', 'b', 'c'],
  );
  assert.deepEqual(
    filterRules(rows, { entity: '', keyword: '', onlyEnabled: true }).map((item) => item.id),
    ['a', 'c'],
  );
  assert.deepEqual(
    filterRules(rows, { entity: 'STOCK_IN', keyword: '', onlyEnabled: false }).map((item) => item.id),
    ['a', 'b'],
  );
  assert.deepEqual(
    filterRules(rows, { entity: '', keyword: 'generate_voucher', onlyEnabled: false }).map((item) => item.id),
    ['b'],
  );
  assert.deepEqual(
    filterRules(rows, { entity: '', keyword: '任务完成', onlyEnabled: false }).map((item) => item.id),
    ['c'],
  );
});

test('参数摘要 / 事务与等级文案 / 健康判定', () => {
  assert.equal(formatParams({}), '—');
  assert.equal(formatParams({ biz_type: 'STOCK_IN', ids: ['a', 'b'], mode: '' }), 'biz_type=STOCK_IN, ids=a|b');
  assert.equal(phaseLabel('IN_TX'), '同事务执行');
  assert.equal(phaseLabel('WHATEVER'), 'WHATEVER', '未知值原样返回，不吞');
  assert.equal(severityLabel('CRITICAL'), '关键（失败回滚）');
  assert.equal(isHealthy(rule({})), true);
  assert.equal(isHealthy(rule({ problems: ['动作未接管'] })), false);
});

test('预览提醒：空执行链 / 回滚风险 / 全部提交后执行', () => {
  const empty = previewHighlights({
    trigger_entity: 'STOCK_IN',
    trigger_entity_label: '收货入库单',
    trigger_status: 'POSTED',
    trigger_wired: true,
    enabled_count: 0,
    steps: [],
    rollback_risk: false,
    after_commit_count: 0,
    warnings: ['当前没有启用的规则'],
    next_run_hint: '不回放历史单据',
    draft: null,
  });
  assert.deepEqual(empty, ['当前没有启用的规则']);

  const risky = previewHighlights({
    trigger_entity: 'STOCK_IN',
    trigger_entity_label: '收货入库单',
    trigger_status: 'POSTED',
    trigger_wired: true,
    enabled_count: 1,
    steps: [
      {
        rule_id: 'r',
        name: 'n',
        seq: 0,
        action_code: 'GENERATE_VOUCHER',
        action_label: '生成凭证',
        phase: 'IN_TX',
        severity: 'CRITICAL',
        action_params: {},
        effect: 'voucher::create_by_rule',
        problems: [],
      },
    ],
    rollback_risk: true,
    after_commit_count: 0,
    warnings: [],
    next_run_hint: '',
    draft: null,
  });
  assert.equal(risky.some((item) => item.includes('回滚')), true);

  assert.equal(draftAlertTheme(null), 'info');
  assert.equal(draftAlertTheme({ ok: true }), 'success');
  assert.equal(draftAlertTheme({ ok: false }), 'error');
});
