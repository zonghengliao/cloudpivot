/**
 * 状态机边配置面板的纯逻辑单测（卡 t_a392a5da）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 断言口径全部抄后端，不抄卡面：
 * - 权限码：`edge_admin.rs::{PERM_LIST, PERM_MANAGE}` == `V0206` 种进 `sys_menu.auth_code` 的值；
 * - `confirm = true` 是硬门（缺了 400 / `10001`，真机实测报文见卡面）；
 * - `mandatory` 边禁不掉、`requires_reason` 只能加不能减（`state_machine::upsert_edge_config`）；
 * - `base_edges` 里 `configured` 恒 false / `enabled` 恒 true —— 页面必须靠 `configs` +
 *   `effective_edges` 叠加，这里用真实响应形状（9 实体 / STOCK_OUT 6 条边）钉住叠加规则。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { EntityStateEdges, StateEdge, StateEdgeConfigRow } from '@/api/business/flow/state-edge';
import {
  MIN_CHANGE_REASON_CHARS,
  STATE_EDGE_PERMISSIONS,
  buildChangeReq,
  changeFormOf,
  changeReasonError,
  changeResultText,
  changeSummary,
  criticalImpactText,
  edgeRowView,
  mergeBaseEdges,
  needsCriticalConfirm,
  pendingChangeText,
} from '@/views/business/flow/def/state-edge-logic';

/** 造一条 BASE 边（形状 = `EdgeRes::of_base`：configured 恒 false、enabled 恒 true） */
function baseEdge(
  from: string,
  to: string,
  label: string,
  mandatory: boolean,
  requiresReason = false,
  criticalImpact: string | null = null,
): StateEdge {
  return {
    from,
    to,
    label,
    mandatory,
    requires_reason: requiresReason,
    auth_code: null,
    configured: false,
    enabled: true,
    critical: criticalImpact !== null,
    critical_impact: criticalImpact,
  };
}

/** STOCK_OUT 的 6 条 BASE 边（顺序 / 文案 / mandatory 抄 2026-11-03 真机 GET 的响应） */
function stockOutBaseEdges(): StateEdge[] {
  return [
    baseEdge('DRAFT', 'PENDING_POST', '提交待过账', true, false, '销售出库无法提交过账审批'),
    baseEdge('PENDING_POST', 'POSTED', '过账', true, false, '销售出库无法过账'),
    baseEdge('PENDING_POST', 'DRAFT', '撤回', true),
    // 【E2E-FIX-1】这条边就是 E2E-02 演练被关掉、把整条销售链断在发货的那一条
    baseEdge('DRAFT', 'POSTED', '直接过账', false, false, '销售出库无法直接过账'),
    baseEdge('DRAFT', 'CANCELLED', '草稿作废', true),
    baseEdge('POSTED', 'CANCELLED', '已过账作废', true, true),
  ];
}

/** 造一个实体视图（默认：没有任何配置行，全部边都生效） */
function entityView(overrides: Partial<EntityStateEdges> = {}): EntityStateEdges {
  const base = overrides.base_edges ?? stockOutBaseEdges();
  return {
    entity: 'STOCK_OUT',
    entity_label: '出库单',
    base_edges: base,
    configs: [],
    effective_edges: base.map((edge) => ({ ...edge })),
    pending_changes: [],
    ...overrides,
  };
}

test('权限码与 edge_admin.rs / V0206 逐字一致', () => {
  assert.equal(STATE_EDGE_PERMISSIONS.list, 'business:flow:state-edge:list');
  assert.equal(STATE_EDGE_PERMISSIONS.manage, 'business:flow:state-edge:manage');
  // 读和改必须是两个码：只授 list 的人不该看到「变更」按钮
  assert.notEqual(STATE_EDGE_PERMISSIONS.list, STATE_EDGE_PERMISSIONS.manage);
});

test('mergeBaseEdges：没有配置行时原样用 BASE 边（configured=false / enabled=true）', () => {
  const rows = mergeBaseEdges(entityView());
  assert.equal(rows.length, 6);
  assert.equal(rows[0].label, '提交待过账');
  assert.equal(rows[0].configured, false);
  assert.equal(rows[0].enabled, true);
  assert.equal(rows[0].auth_code, null);
  // BASE 边里 requires_reason 只有「已过账作废」是 true
  assert.deepEqual(
    rows.filter((row) => row.requires_reason).map((row) => `${row.from}>${row.to}`),
    ['POSTED>CANCELLED'],
  );
});

test('mergeBaseEdges：有配置行 → configured=true，文案 / 权限码 / 启用取配置值', () => {
  const config: StateEdgeConfigRow = {
    entity: 'STOCK_OUT',
    from: 'DRAFT',
    to: 'POSTED',
    enabled: false,
    requires_reason: true,
    label: '直接过账（需主管批）',
    auth_code: 'business:erp:stock-out:direct-post',
  };
  // 被禁用的边不会出现在 effective_edges 里（后端 effective_edges 只返回启用中的边）
  const base = stockOutBaseEdges();
  const view = entityView({
    configs: [config],
    effective_edges: base
      .filter((edge) => !(edge.from === 'DRAFT' && edge.to === 'POSTED'))
      .map((edge) => ({ ...edge })),
  });
  const rows = mergeBaseEdges(view);
  const row = rows.find((item) => item.from === 'DRAFT' && item.to === 'POSTED');
  assert.ok(row);
  assert.equal(row.configured, true);
  assert.equal(row.enabled, false);
  assert.equal(row.requires_reason, true);
  assert.equal(row.label, '直接过账（需主管批）');
  assert.equal(row.auth_code, 'business:erp:stock-out:direct-post');
  // 其它边不受影响
  assert.equal(rows.filter((item) => item.configured).length, 1);
});

test('mergeBaseEdges：requires_reason 只能加不能减（BASE true 时配置 false 也算 true）', () => {
  const config: StateEdgeConfigRow = {
    entity: 'STOCK_OUT',
    from: 'POSTED',
    to: 'CANCELLED',
    enabled: true,
    requires_reason: false, // 内核不会让这种行存在；前端仍按「或」取，不能把 BASE 的要求抹掉
    label: null,
    auth_code: null,
  };
  const rows = mergeBaseEdges(entityView({ configs: [config] }));
  const row = rows.find((item) => item.from === 'POSTED' && item.to === 'CANCELLED');
  assert.ok(row);
  assert.equal(row.requires_reason, true);
  // label 为空 → 回落 BASE 文案
  assert.equal(row.label, '已过账作废');
});

test('mergeBaseEdges：配置行缺失但边不在 effective_edges 里 → 视为已禁用', () => {
  const base = stockOutBaseEdges();
  const view = entityView({
    effective_edges: base
      .filter((edge) => !(edge.from === 'DRAFT' && edge.to === 'CANCELLED'))
      .map((edge) => ({ ...edge })),
  });
  const row = mergeBaseEdges(view).find((item) => item.from === 'DRAFT' && item.to === 'CANCELLED');
  assert.ok(row);
  assert.equal(row.enabled, false);
  assert.equal(row.configured, false);
});

test('edgeRowView：mandatory 边禁不掉、BASE 要求原因的边开关锁死', () => {
  const rows = mergeBaseEdges(entityView()).map((edge) => edgeRowView('STOCK_OUT', edge));
  const mandatoryRow = rows.find((row) => row.from === 'PENDING_POST' && row.to === 'POSTED');
  assert.ok(mandatoryRow);
  assert.equal(mandatoryRow.mandatoryText, '硬约束');
  assert.equal(mandatoryRow.canDisable, false);

  const optionalRow = rows.find((row) => row.from === 'DRAFT' && row.to === 'POSTED');
  assert.ok(optionalRow);
  assert.equal(optionalRow.mandatoryText, '可选');
  assert.equal(optionalRow.canDisable, true);

  const reasonRow = rows.find((row) => row.from === 'POSTED' && row.to === 'CANCELLED');
  assert.ok(reasonRow);
  assert.equal(reasonRow.reasonText, '必须填原因');
  assert.equal(reasonRow.reasonLocked, true);

  // 行 key 跨实体唯一
  assert.equal(rows[0].key, 'STOCK_OUT:DRAFT>PENDING_POST');
});

test('changeFormOf：没有配置行时文案回填空串（不把 BASE 文案写死进库）', () => {
  const rows = mergeBaseEdges(entityView());
  const plain = rows[0];
  assert.equal(changeFormOf(plain).label, '');

  const configured = { ...plain, configured: true, label: '提交（财务先看）' };
  assert.equal(changeFormOf(configured).label, '提交（财务先看）');
});

test('buildChangeReq：confirm 恒为 true、空串归一成 null、理由 trim', () => {
  const edge = mergeBaseEdges(entityView())[3]; // DRAFT > POSTED（可选边）
  const req = buildChangeReq('STOCK_OUT', edge, {
    enabled: false,
    requires_reason: true,
    label: '   ',
    auth_code: '',
    reason: '  业务要求收紧直接过账  ',
  });
  assert.equal(req.confirm, true);
  assert.equal(req.entity, 'STOCK_OUT');
  // 请求体用 from_status / to_status（不是响应里的 from / to）
  assert.equal(req.from_status, 'DRAFT');
  assert.equal(req.to_status, 'POSTED');
  assert.equal(req.enabled, false);
  assert.equal(req.requires_reason, true);
  assert.equal(req.label, null);
  assert.equal(req.auth_code, null);
  assert.equal(req.reason, '业务要求收紧直接过账');
  // 不该把响应字段名漏进请求体
  assert.equal((req as unknown as Record<string, unknown>).from, undefined);
  assert.equal((req as unknown as Record<string, unknown>).to, undefined);
});

test('buildChangeReq：BASE 要求原因的边，即使表单传 false 也必须提交 true', () => {
  const edge = mergeBaseEdges(entityView()).find((item) => item.from === 'POSTED' && item.to === 'CANCELLED');
  assert.ok(edge);
  const req = buildChangeReq('STOCK_OUT', edge, {
    enabled: true,
    requires_reason: false, // 界面开关锁死时不会发生；这里直接钉住兜底口径
    label: '',
    auth_code: '',
    reason: '复核一下',
  });
  assert.equal(req.requires_reason, true);
});

test('【E2E-FIX-1】needsCriticalConfirm：只有「禁用 + 主链路必需边」才要确认', () => {
  const rows = mergeBaseEdges(entityView());
  const criticalEdge = rows.find((edge) => edge.from === 'DRAFT' && edge.to === 'POSTED');
  const plainEdge = rows.find((edge) => edge.from === 'DRAFT' && edge.to === 'CANCELLED');
  assert.ok(criticalEdge && plainEdge);
  assert.equal(criticalEdge.critical, true);
  assert.equal(criticalEdge.critical_impact, '销售出库无法直接过账');

  // 禁用主链路边 → 要确认
  assert.equal(needsCriticalConfirm(criticalEdge, { enabled: false }), true);
  // 启用 / 只是改文案 → 不要确认
  assert.equal(needsCriticalConfirm(criticalEdge, { enabled: true }), false);
  // 非主链路边禁用 → 不要确认（本来就能关）
  assert.equal(needsCriticalConfirm(plainEdge, { enabled: false }), false);
});

test('【E2E-FIX-1】buildChangeReq：禁用主链路边必须带 confirm_critical', () => {
  const rows = mergeBaseEdges(entityView());
  const criticalEdge = rows.find((edge) => edge.from === 'DRAFT' && edge.to === 'POSTED');
  const plainEdge = rows.find((edge) => edge.from === 'DRAFT' && edge.to === 'CANCELLED');
  assert.ok(criticalEdge && plainEdge);
  const form = { enabled: false, requires_reason: false, label: '', auth_code: '', reason: '演练需要' };

  // ① 没勾确认 → 仍然提交，但 confirm_critical = false（后端 400，点名业务后果）
  assert.equal(buildChangeReq('STOCK_OUT', criticalEdge, form).confirm_critical, false);
  // ② 勾了确认 → true（后端才放行）
  assert.equal(buildChangeReq('STOCK_OUT', criticalEdge, form, true).confirm_critical, true);
  // ③ 重新启用主链路边 → 不带这个字段（确认过了也不许写进审批单，免得误导复核人）
  const enabling = { ...form, enabled: true };
  assert.equal(buildChangeReq('STOCK_OUT', criticalEdge, enabling, true).confirm_critical, undefined);
  // ④ 非主链路边 → 不带
  assert.equal(buildChangeReq('STOCK_OUT', plainEdge, form, true).confirm_critical, undefined);
});

test('【E2E-FIX-1】edgeRowView / changeSummary：主链路边有红标，禁用时摘要点名业务后果', () => {
  const rows = mergeBaseEdges(entityView()).map((edge) => edgeRowView('STOCK_OUT', edge));
  const criticalRow = rows.find((row) => row.from === 'DRAFT' && row.to === 'POSTED');
  const plainRow = rows.find((row) => row.from === 'DRAFT' && row.to === 'CANCELLED');
  assert.ok(criticalRow && plainRow);
  assert.equal(criticalRow.criticalText, '主链路');
  assert.equal(criticalRow.criticalTheme, 'danger');
  assert.equal(criticalRow.criticalImpact, '销售出库无法直接过账');
  assert.equal(plainRow.criticalText, '—');
  assert.equal(plainRow.criticalTheme, 'default');

  const disabling = changeSummary(criticalRow.edge, {
    enabled: false,
    requires_reason: false,
    label: '',
    auth_code: '',
    reason: 'x',
  });
  assert.match(disabling, /主链路必需边/);
  assert.match(disabling, /销售出库无法直接过账/);

  // 启用时不带警告（改配置不等于断链）
  const enabling = changeSummary(criticalRow.edge, {
    enabled: true,
    requires_reason: false,
    label: '',
    auth_code: '',
    reason: 'x',
  });
  assert.doesNotMatch(enabling, /主链路必需边/);

  // 字段缺失（老后端）时退化成通用话，绝不渲染成 "undefined"
  assert.equal(criticalImpactText({ ...criticalRow.edge, critical_impact: null }), '关掉它会让业务功能断');
});

test('changeReasonError：理由不能空、不能太短', () => {
  assert.notEqual(changeReasonError(''), null);
  assert.notEqual(changeReasonError('   '), null);
  assert.notEqual(changeReasonError('a'.repeat(MIN_CHANGE_REASON_CHARS - 1)), null);
  assert.equal(changeReasonError('a'.repeat(MIN_CHANGE_REASON_CHARS)), null);
  assert.equal(changeReasonError('  业务要求收紧  '), null);
});

test('changeSummary / changeResultText / pendingChangeText：文案分开「已送审」与「已生效」', () => {
  const edge = mergeBaseEdges(entityView())[3];
  const summary = changeSummary(edge, {
    enabled: false,
    requires_reason: false,
    label: '直接过账（停用）',
    auth_code: '',
    reason: 'x',
  });
  assert.match(summary, /禁用/);
  assert.match(summary, /文案改为「直接过账（停用）」/);

  const pending = changeResultText({
    applied: false,
    instance_no: 'FLW-20261103-000001',
    message: '启用：已提交审批 FLW-20261103-000001，审批通过后才会生效（驳回或撤回则配置不变）',
  });
  assert.match(pending, /已提交审批/);
  assert.match(pending, /审批通过后才会生效/);

  const applied = changeResultText({ applied: true, instance_no: null, message: '禁用：已直接生效（流程已下线）' });
  assert.match(applied, /已直接生效/);

  const line = pendingChangeText({
    instance_id: 'i-1',
    instance_no: 'FLW-20261103-000002',
    title: '状态机配置变更',
    action: 'UPSERT',
    entity: 'STOCK_OUT',
    from: 'DRAFT',
    to: 'POSTED',
    summary: '禁用（变成 false）',
    initiator_name: '管理员',
    started_at: '2026-11-03T03:00:00Z',
  });
  assert.match(line, /FLW-20261103-000002/);
  assert.match(line, /DRAFT → POSTED/);
  assert.match(line, /管理员 发起/);
});
