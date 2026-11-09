-- V0361 down：撤销收款退款单的审批实例弱引用（E2E-FIX-8 ②）
-- 列是本迁移新增的唯一结构；审批实例本身在 biz_flow_instance，不动。
DROP INDEX IF EXISTS ix_biz_receipt_refund_approval_instance;

ALTER TABLE biz_receipt_refund
    DROP COLUMN IF EXISTS approval_instance_id;
