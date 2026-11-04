/**
 * 凭证审核链（审核 / 反审核 / 记账）状态矩阵单测（P0-FIX-4B）。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么测 `.ts` 不测 `.vue`：这三个按钮**决定账动不动**——
 * 「草稿能不能直接记账」「记账后能不能反审核」「制单人能不能自己审自己」
 * 每一格错了都是内控事故，而它们全是纯函数（`voucher-audit-logic.ts`），
 * 不需要 DOM 就能把状态矩阵钉死。
 *
 * 断言口径与后端 P0-FIX-4A 对齐（PRD E4-09 / E4-10）：
 * ```text
 * DRAFT    → 审核     （DRAFT    → AUDITED）
 * AUDITED  → 反审核   （AUDITED  → DRAFT，原因必填）
 * AUDITED  → 记账     （AUDITED  → POSTED）
 * POSTED   → 无操作   （更正只能红冲）
 * CANCELLED→ 无操作
 * ```
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  SELF_AUDIT_BLOCKED_REASON,
  UNAUDIT_REASON_MIN_LENGTH,
  VOUCHER_ACTION_SPECS,
  allowedVoucherActions,
  isSelfAudit,
  permissionBlockedReason,
  unAuditReasonError,
  voucherActionButtons,
  voucherActionConfirmText,
  voucherActionDoneText,
  voucherApprovalInstanceId,
  voucherAuditNote,
} from '@/views/business/erp/voucher/voucher-audit-logic';

/** 全权限 + 无自审：绝大多数用例的基线上下文 */
const ALLOW_ALL = {
  hasPermission: () => true,
  currentUserId: 'u-reviewer',
  createdBy: 'u-maker',
};

/** 只测「状态 → 动作清单」时用的上下文（权限全开） */
function keysOf(status: string | null | undefined): string[] {
  return allowedVoucherActions(status).join(',') ? allowedVoucherActions(status).slice() : [];
}

test('状态 → 允许的动作：草稿只有审核', () => {
  assert.deepEqual(keysOf('DRAFT'), ['audit']);
});

test('状态 → 允许的动作：已审核有反审核 + 记账（顺序即界面顺序）', () => {
  assert.deepEqual(keysOf('AUDITED'), ['un-audit', 'post']);
});

test('状态 → 允许的动作：已记账 / 已作废 / 未知状态一个都没有', () => {
  for (const status of ['POSTED', 'CANCELLED', '', null, undefined, 'WHAT']) {
    assert.deepEqual(allowedVoucherActions(status), [], `状态 ${String(status)} 不该有动作`);
  }
});

test('动作声明：权限码与后端 #[alun::permission] 同名，且只有反审核要求原因', () => {
  assert.equal(VOUCHER_ACTION_SPECS.audit.permission, 'business:erp:voucher:audit');
  assert.equal(VOUCHER_ACTION_SPECS['un-audit'].permission, 'business:erp:voucher:un-audit');
  assert.equal(VOUCHER_ACTION_SPECS.post.permission, 'business:erp:voucher:post');
  assert.equal(VOUCHER_ACTION_SPECS.audit.requiresReason, false);
  assert.equal(VOUCHER_ACTION_SPECS.post.requiresReason, false);
  assert.equal(VOUCHER_ACTION_SPECS['un-audit'].requiresReason, true);
  // 落点状态（状态机里这三条边必须存在，否则后端会 409）
  assert.equal(VOUCHER_ACTION_SPECS.audit.to, 'AUDITED');
  assert.equal(VOUCHER_ACTION_SPECS['un-audit'].to, 'DRAFT');
  assert.equal(VOUCHER_ACTION_SPECS.post.to, 'POSTED');
});

test('按钮清单：全权限 + 无自审 → 都可点且无置灰原因', () => {
  const draft = voucherActionButtons('DRAFT', ALLOW_ALL);
  assert.deepEqual(draft.map((b) => b.key), ['audit']);
  assert.equal(draft[0].disabled, false);
  assert.equal(draft[0].reason, '');

  const audited = voucherActionButtons('AUDITED', ALLOW_ALL);
  assert.deepEqual(audited.map((b) => b.key), ['un-audit', 'post']);
  assert.ok(audited.every((b) => !b.disabled && b.reason === ''));
});

test('按钮清单：没权限 → 置灰并点名缺哪个权限码（不隐藏）', () => {
  const buttons = voucherActionButtons('AUDITED', {
    ...ALLOW_ALL,
    hasPermission: (code: string) => code === 'business:erp:voucher:post',
  });
  const unAudit = buttons.find((b) => b.key === 'un-audit');
  const post = buttons.find((b) => b.key === 'post');
  assert.equal(unAudit?.disabled, true);
  assert.equal(unAudit?.reason, permissionBlockedReason('business:erp:voucher:un-audit'));
  assert.match(unAudit?.reason ?? '', /business:erp:voucher:un-audit/);
  assert.equal(post?.disabled, false);
  assert.equal(post?.reason, '');
});

test('按钮清单：制单人自审 → 审核置灰（内控），反审核/记账不受影响', () => {
  const same = { hasPermission: () => true, currentUserId: 'u-1', createdBy: 'u-1' };
  const draft = voucherActionButtons('DRAFT', same);
  assert.equal(draft[0].disabled, true);
  assert.equal(draft[0].reason, SELF_AUDIT_BLOCKED_REASON);

  // 制单人是别人 → 可审
  const other = voucherActionButtons('DRAFT', { ...same, createdBy: 'u-2' });
  assert.equal(other[0].disabled, false);
});

test('自审判定：两个 id 任一未知 → 不拦（宁可不拦，也不误拦）', () => {
  assert.equal(isSelfAudit({ currentUserId: null, createdBy: 'u-1' }), false);
  assert.equal(isSelfAudit({ currentUserId: 'u-1', createdBy: null }), false);
  assert.equal(isSelfAudit({ currentUserId: '', createdBy: '' }), false);
  assert.equal(isSelfAudit({}), false);
  assert.equal(isSelfAudit({ currentUserId: 'u-1', createdBy: 'u-1' }), true);
  assert.equal(isSelfAudit({ currentUserId: ' u-1 ', createdBy: 'u-1' }), true);
});

test('反审核原因必填：空白 / 只有空格 / null / undefined 全部拦下', () => {
  assert.equal(UNAUDIT_REASON_MIN_LENGTH, 1);
  for (const reason of ['', '   ', '\n\t ', null, undefined]) {
    assert.notEqual(unAuditReasonError(reason), '', `「${String(reason)}」不该通过`);
  }
  assert.equal(unAuditReasonError('科目选错'), '');
  assert.equal(unAuditReasonError('  科目选错  '), '');
});

test('审核链说明文案：每个状态都说清「为什么没有按钮 / 下一步走哪」', () => {
  assert.match(voucherAuditNote('DRAFT'), /审核/);
  assert.match(voucherAuditNote('AUDITED'), /反审核/);
  assert.match(voucherAuditNote('AUDITED'), /记账/);
  assert.match(voucherAuditNote('POSTED'), /红冲/); // 已记账 → 只能红冲
  assert.match(voucherAuditNote('CANCELLED'), /作废/);
  assert.equal(voucherAuditNote('WHAT'), '');
});

test('动作回执 / 确认文案：记账要写明「不可再反审核」', () => {
  assert.match(voucherActionDoneText('post', 'PZ-001'), /不可反审核/);
  assert.match(voucherActionDoneText('un-audit', 'PZ-001'), /草稿/);
  assert.match(voucherActionDoneText('audit', 'PZ-001'), /已审核/);
  assert.match(voucherActionConfirmText('post', 'PZ-001'), /红冲/);
  assert.match(voucherActionConfirmText('audit', 'PZ-001'), /PZ-001/);
});

test('审批实例 id：红冲实例优先，其次审核实例，都没有 → null（进度块不渲染）', () => {
  assert.equal(
    voucherApprovalInstanceId({ pending_reverse_instance_id: 'FLW-R', approval_instance_id: 'FLW-A' }),
    'FLW-R',
  );
  assert.equal(
    voucherApprovalInstanceId({ pending_reverse_instance_id: null, approval_instance_id: 'FLW-A' }),
    'FLW-A',
  );
  assert.equal(
    voucherApprovalInstanceId({ pending_reverse_instance_id: '', approval_instance_id: '' }),
    null,
  );
  assert.equal(voucherApprovalInstanceId({}), null);
  assert.equal(voucherApprovalInstanceId(null), null);
  assert.equal(voucherApprovalInstanceId(undefined), null);
});
