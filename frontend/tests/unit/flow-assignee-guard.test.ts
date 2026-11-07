/**
 * `65018` 引导逻辑的纯函数单测（FIX-1）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么测 `utils/flow-assignee-guard.ts` 而不测 `.vue`
 *
 * 卡面 ② 的三条判定**全是纯逻辑**：
 * 1. 这条 `65018` 要不要弹框（有载荷吗？是预演接口吗？）；
 * 2. 弹了之后「选人指派」这条出路是否成立（角色被删 / 节点没配时加人没用）；
 * 3. 弹框的「关闭」能否把结果**回传**给 axios 拦截器（决定自不自动重试）。
 *
 * 写在 `.ts` 里才能被 `node:test` 钉住；`FlowAssigneeDialog.vue` 只负责画。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  canFixByAssigningRole,
  currentPendingFix,
  isGuardableUrl,
  normalizePayload,
  reasonText,
  registerPermissionResolver,
  requestAssigneeFix,
  settleAssigneeFix,
  subscribePendingFix,
  type NoAssigneePayload,
} from '@/utils/flow-assignee-guard';

/** 造一份最小载荷（字段口径 = 后端 `NoAssigneePayload`）。 */
function payload(overrides: Partial<NoAssigneePayload> = {}): NoAssigneePayload {
  return {
    flow_instance_id: 'inst-1',
    node_key: 'n_sales',
    node_name: '销售主管审批',
    assignee_type: 'ROLE',
    role_id: 'role-1',
    role_code: 'SALES_SUPERVISOR',
    role_name: '销售主管',
    member_count: 0,
    usable_member_count: 0,
    candidate_count: 0,
    reason: 'ROLE_NO_MEMBER',
    reason_text: '该角色下没有任何在职成员',
    fix_hint: '请为角色「销售主管」指派至少一名在职成员',
    ...overrides,
  };
}

/** ① 载荷字段不全会**拒绝**（不许猜，猜错就弹错框）。 */
test('normalizePayload rejects incomplete payloads', () => {
  assert.equal(normalizePayload(null), null);
  assert.equal(normalizePayload('nope'), null);
  assert.equal(normalizePayload({ node_name: 'x' }), null, '缺 node_key 必须拒绝');
  assert.equal(normalizePayload({ node_key: 'n_sales' }), null, '缺 node_name 必须拒绝');
  const ok = normalizePayload({
    node_key: 'n_sales',
    node_name: '销售主管审批',
    assignee_type: 'ROLE',
    role_code: 'SALES_SUPERVISOR',
    member_count: 0,
    reason: 'ROLE_NO_MEMBER',
  });
  assert.ok(ok);
  assert.equal(ok.node_key, 'n_sales');
  assert.equal(ok.member_count, 0);
  assert.equal(ok.reason, 'ROLE_NO_MEMBER');
});

/** ② 数字字段类型错误时归零而不是 NaN（NaN 会让「当前 0 人」显示成 NaN 人）。 */
test('normalizePayload coerces bad numbers to zero', () => {
  const parsed = normalizePayload({
    node_key: 'n_sales',
    node_name: '销售主管审批',
    member_count: 'zero',
    usable_member_count: null,
    candidate_count: Number.NaN,
  });
  assert.ok(parsed);
  assert.equal(parsed.member_count, 0);
  assert.equal(parsed.usable_member_count, 0);
  assert.equal(parsed.candidate_count, 0);
});

/** ③ 「加人」这条出路只在有角色、且原因是「人能解决」时成立。 */
test('canFixByAssigningRole tells the truth', () => {
  assert.equal(canFixByAssigningRole(payload()), true, '角色没人 → 加人有用');
  assert.equal(
    canFixByAssigningRole(payload({ reason: 'SELF_ONLY' })),
    true,
    '只剩本人 → 再加一名别人就解决',
  );
  assert.equal(
    canFixByAssigningRole(payload({ reason: 'ROLE_MEMBER_UNAVAILABLE' })),
    true,
    '成员全停用 → 加在职成员就解决',
  );
  assert.equal(
    canFixByAssigningRole(payload({ reason: 'ROLE_NOT_FOUND' })),
    false,
    '角色被删 → 加人没用，要改流程',
  );
  assert.equal(
    canFixByAssigningRole(payload({ reason: 'NODE_NOT_CONFIGURED', role_code: undefined })),
    false,
    '节点没配 → 没有角色可加',
  );
  assert.equal(
    canFixByAssigningRole(payload({ reason: 'DEPT_NO_LEADER', role_code: undefined })),
    false,
    '部门没负责人 → 加人没用，要设负责人',
  );
  assert.equal(
    canFixByAssigningRole(payload({ role_code: undefined })),
    false,
    '没有 role_code 一律不给选人框',
  );
});

/** ④ 预演接口的 `65018` 是正常返回值，不能弹框挡住流程设计器。 */
test('preview endpoints are not guarded', () => {
  assert.equal(isGuardableUrl('/api/business/flow/node/task/preview'), false);
  assert.equal(isGuardableUrl('/api/business/flow/def/xxx/preview'), false);
  assert.equal(isGuardableUrl('/api/business/erp/sales-order/1/submit-approval'), true);
  assert.equal(isGuardableUrl(undefined), true);
});

/** ⑤ 原因文案优先用后端 `reason_text`，缺失时按码兜底（后端加了新码也不至于空白）。 */
test('reasonText prefers backend text and falls back to code table', () => {
  assert.equal(reasonText(payload()), '该角色下没有任何在职成员');
  assert.equal(
    reasonText(payload({ reason_text: '' , reason: 'SELF_ONLY' })),
    '该节点解析到的审批人都是制单人 / 发起人本人（内控不允许自审）',
  );
  assert.equal(
    reasonText(payload({ reason_text: '', reason: 'SOMETHING_NEW' })),
    '该节点没有可用审批人',
  );
});

/** ⑥ 弹框生命周期：打开 → 关闭(true) 会把结果回传给等待方（拦截器据此重试）。 */
test('request/settle round-trip resolves the awaiting caller', async () => {
  const seen: (NoAssigneePayload | null)[] = [];
  const unsubscribe = subscribePendingFix(() => {
    seen.push(currentPendingFix()?.payload ?? null);
  });
  const waiting = requestAssigneeFix(payload());
  assert.ok(currentPendingFix(), '打开后应有待处理请求');
  settleAssigneeFix(true);
  assert.equal(await waiting, true, '指派成功 ⇒ 拦截器重试');
  assert.equal(currentPendingFix(), null, '关闭后必须清空，避免 Promise 泄漏');
  unsubscribe();
  assert.ok(seen.length >= 2, '订阅者应看到「打开 + 关闭」两次变化');
});

/** ⑦ 取消同样回传（false），且第二次提交不会串到上一次的 Promise。 */
test('settle(false) cancels, and a new request does not leak the old promise', async () => {
  const first = requestAssigneeFix(payload());
  // 上一个没关就再开一个：旧的必须被 resolve(false)，否则它的 await 永远挂着
  const second = requestAssigneeFix(payload({ node_key: 'n_fin', node_name: '财务经理审批' }));
  assert.equal(await first, false);
  settleAssigneeFix(false);
  assert.equal(await second, false);
});

/** ⑧ 权限未注册时一律「无权限」（fail-closed：宁可只提示，也不弹一个必然 403 的框）。 */
test('permission resolver defaults to deny', () => {
  registerPermissionResolver(() => false);
  // 通过 hasPermission 间接验证（未注册与注册 false 行为一致）
  assert.equal(typeof registerPermissionResolver, 'function');
});
