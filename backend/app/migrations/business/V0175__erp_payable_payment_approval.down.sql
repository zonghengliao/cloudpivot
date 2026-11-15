-- ======================================================
-- V0175 回滚：应付付款审批
--   ① 残留的「审批中」付款退回未发起（PENDING 的行无法在无审批能力的世界里等回调）
--   ② 删种子流程（只删固定 UUID 那一套：def / version / node / edge）
--   ③ 删索引 / 约束 / 列（biz_payment 与 biz_payable）
-- 全程可重复执行（IF EXISTS / 条件 UPDATE），零残留。
-- ======================================================

-- ---------- 0) 残留数据先回退 ----------
-- 付款审批这个能力不存在了：停在审批中的付款行打回 NONE（**账本来就没动**，不需要做任何冲销），
-- 付款单号与付款记录保留（审计不丢），只摘掉审批引用。
UPDATE biz_payment
   SET approval_status = 'NONE', approval_instance_id = NULL, updated_at = now()
 WHERE approval_status = 'PENDING';

UPDATE biz_payable
   SET payment_approval_instance_id = NULL, updated_at = now()
 WHERE payment_approval_instance_id IS NOT NULL;

-- ---------- 1) 删种子流程 ----------
DELETE FROM biz_flow_edge
 WHERE version_id = '20000000-0000-4000-8000-000000000718';

DELETE FROM biz_flow_node
 WHERE version_id = '20000000-0000-4000-8000-000000000718';

-- 定义行先摘掉 current 版本指针（避免删版本行时撞引用）
UPDATE biz_flow_def
   SET current_version_id = NULL, current_version_no = 0
 WHERE id = '20000000-0000-4000-8000-000000000717';

DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-000000000718';

DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-000000000717';

-- ---------- 2) 索引 / 约束 ----------
DROP INDEX IF EXISTS idx_biz_payment_approval;
DROP INDEX IF EXISTS uk_biz_payment_pending;
DROP INDEX IF EXISTS idx_biz_payable_payment_approval;

ALTER TABLE biz_payment DROP CONSTRAINT IF EXISTS ck_biz_payment_pending_instance;
ALTER TABLE biz_payment DROP CONSTRAINT IF EXISTS ck_biz_payment_approval_status;

-- ---------- 3) 列 ----------
ALTER TABLE biz_payment DROP COLUMN IF EXISTS approval_instance_id;
ALTER TABLE biz_payment DROP COLUMN IF EXISTS approval_status;
ALTER TABLE biz_payable DROP COLUMN IF EXISTS payment_approval_instance_id;
