-- V0191 回滚：财务链两条标准审批流（P-FLOW-3B）
-- ⚠️ 已用这两条流程发起过的实例不会被删（实例是审计数据）；
--    回滚后 `payable_bridge` / `voucher_bridge` 按 biz_type 查不到流程，
--    提交入口自动降级为「直接作废 / 直接红冲」（与 P-FLOW-3A 之前一致）。

-- 权限码
DELETE FROM sys_role_menu WHERE menu_id IN (
    '20000000-0000-0000-0000-000000001028'::uuid,
    '20000000-0000-0000-0000-000000001029'::uuid,
    '20000000-0000-0000-0000-000000001030'::uuid,
    '20000000-0000-0000-0000-000000001031'::uuid);
DELETE FROM sys_menu WHERE id IN (
    '20000000-0000-0000-0000-000000001031'::uuid,
    '20000000-0000-0000-0000-000000001030'::uuid,
    '20000000-0000-0000-0000-000000001029'::uuid,
    '20000000-0000-0000-0000-000000001028'::uuid);

-- 流程定义（版本 → 节点/边/审批人 级联删）
DELETE FROM biz_flow_node_assignee WHERE node_id IN (
    SELECT id FROM biz_flow_node WHERE version_id IN (
        '20000000-0000-4000-8000-0000000c0183'::uuid,
        '20000000-0000-4000-8000-0000000d0183'::uuid));
DELETE FROM biz_flow_edge WHERE version_id IN (
    '20000000-0000-4000-8000-0000000c0183'::uuid,
    '20000000-0000-4000-8000-0000000d0183'::uuid);
DELETE FROM biz_flow_node WHERE version_id IN (
    '20000000-0000-4000-8000-0000000c0183'::uuid,
    '20000000-0000-4000-8000-0000000d0183'::uuid);
DELETE FROM biz_flow_def_version WHERE id IN (
    '20000000-0000-4000-8000-0000000c0183'::uuid,
    '20000000-0000-4000-8000-0000000d0183'::uuid);
DELETE FROM biz_flow_def WHERE id IN (
    '20000000-0000-4000-8000-0000000c0182'::uuid,
    '20000000-0000-4000-8000-0000000d0182'::uuid);

-- 角色（客户可能已经绑了人：先解绑再删）
DELETE FROM sys_user_role WHERE role_id IN (
    '20000000-0000-4000-8000-0000000f0005'::uuid,
    '20000000-0000-4000-8000-0000000f0006'::uuid);
DELETE FROM sys_role_menu WHERE role_id IN (
    '20000000-0000-4000-8000-0000000f0005'::uuid,
    '20000000-0000-4000-8000-0000000f0006'::uuid);
DELETE FROM sys_role WHERE id IN (
    '20000000-0000-4000-8000-0000000f0005'::uuid,
    '20000000-0000-4000-8000-0000000f0006'::uuid);

-- 列与约束（回滚前先把 PENDING_CANCEL 的行还原成 OPEN，否则约束建不回去）
UPDATE biz_payable
   SET status = 'OPEN', cancel_instance_id = NULL, cancel_reason = NULL, updated_at = now()
 WHERE status = 'PENDING_CANCEL';
UPDATE biz_voucher
   SET pending_reverse_instance_id = NULL, reverse_reason = NULL, updated_at = now()
 WHERE pending_reverse_instance_id IS NOT NULL;

ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_pending_reverse;
DROP INDEX IF EXISTS idx_biz_voucher_pending_reverse;
ALTER TABLE biz_voucher
    DROP COLUMN IF EXISTS reverse_reason,
    DROP COLUMN IF EXISTS pending_reverse_instance_id;

ALTER TABLE biz_payable DROP CONSTRAINT IF EXISTS ck_biz_payable_pending_cancel;
ALTER TABLE biz_payable DROP CONSTRAINT IF EXISTS ck_biz_payable_status;
ALTER TABLE biz_payable
    ADD CONSTRAINT ck_biz_payable_status
    CHECK (status IN ('OPEN', 'PARTIAL_PAID', 'SETTLED', 'CANCELLED'));
DROP INDEX IF EXISTS idx_biz_payable_cancel_instance;
ALTER TABLE biz_payable
    DROP COLUMN IF EXISTS cancel_reason,
    DROP COLUMN IF EXISTS cancel_instance_id;
