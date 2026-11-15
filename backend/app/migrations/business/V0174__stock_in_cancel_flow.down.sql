-- ======================================================
-- V0174 回滚：收货单作废审批
--   ① 删种子流程（只删固定 UUID 那一套：def / version / node / edge）
--   ② 摘掉 ck_biz_si_pending_cancel
--   ③ ck_biz_si_status 回退到 3 态（先把残留的 PENDING_CANCEL 落回 POSTED，否则约束建不上）
--   ④ 删索引 + 删列
-- 全程可重复执行（IF EXISTS / 条件 UPDATE），零残留。
-- ======================================================

-- ---------- 0) 残留数据先回退（有 PENDING_CANCEL 单子时不能直接收紧约束） ----------
-- 回滚意味着「作废审批」这个能力不存在了：停在审批中的单据退回已过账（账本来就没动），
-- 挂着实例的弱引用一并清空 —— 与 ck_biz_si_pending_cancel 的语义保持一致。
UPDATE biz_stock_in
   SET status = 'POSTED', approval_instance_id = NULL,
       updated_at = now(), cancel_reason = NULL
 WHERE status = 'PENDING_CANCEL';

-- ---------- 1) 删种子流程 ----------
DELETE FROM biz_flow_edge
 WHERE version_id = '20000000-0000-4000-8000-00000000e174';

DELETE FROM biz_flow_node
 WHERE version_id = '20000000-0000-4000-8000-00000000e174';

-- 定义行先摘掉 current 版本指针（避免删版本行时撞引用）
UPDATE biz_flow_def
   SET current_version_id = NULL, current_version_no = 0
 WHERE id = '20000000-0000-4000-8000-00000000d174';

DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-00000000e174';

DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-00000000d174';

-- ---------- 2) 状态约束回退 ----------
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_pending_cancel;
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_status;
ALTER TABLE biz_stock_in
    ADD CONSTRAINT ck_biz_si_status
    CHECK (status IN ('DRAFT', 'POSTED', 'CANCELLED'));

-- ---------- 3) 删索引 / 列 ----------
DROP INDEX IF EXISTS idx_biz_si_approval;
ALTER TABLE biz_stock_in DROP COLUMN IF EXISTS approval_instance_id;
