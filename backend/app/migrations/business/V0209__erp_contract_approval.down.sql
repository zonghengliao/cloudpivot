-- V0209 down：撤掉合同审批流（ARCH-3 R-P1-1）。
--
-- ⚠️ 回滚前请先确认业务侧不再依赖「合同必须审批才能签订」：
--   · 回滚本迁移**不会**把代码改回「DRAFT 直接点签订」（那是二进制的事）：
--     代码仍然只认 7 条边、仍然要求走 `contract_approve`。
--   · 因此 down 只用于「本迁移误上」的即时回退；正常回滚要连同二进制一起回滚。
--   · 回滚会把**审批中**的合同退成草稿（否则 5 值白名单装不下 PENDING_APPROVAL），
--     在跑的审批实例一并终止（不留孤儿待办），理由写进实例的 comment。
--
-- 顺序：先终止审批实例 → 把审批中的合同退回 DRAFT（清实例）→ 恢复 5 值约束
--       → 删流程四层（assignee / edge / node / version / def）→ 删列。

-- ---------- ① 在跑的合同审批实例：终止（先待办、后节点、再主表） ----------
-- 顺序与 `flow::model::cancel_pending_of_biz` 逐字一致（子表先动，主表最后翻状态；
-- `ended_at` 必须与状态同写，否则撞 `ck_biz_flow_instance_end`）。
UPDATE biz_flow_task t
   SET status = 'CANCELLED',
       comment = COALESCE(t.comment, 'V0209 down：合同审批流回滚，审批实例终止'),
       completed_at = now()
  FROM biz_flow_instance i
 WHERE t.instance_id = i.id AND t.status = 'PENDING'
   AND i.status = 'PENDING' AND i.deleted_at IS NULL
   AND i.biz_type = 'CONTRACT_APPROVE';

UPDATE biz_flow_instance_node n
   SET status = 'CANCELLED', ended_at = now(),
       duration_ms = (EXTRACT(EPOCH FROM (now() - COALESCE(n.activated_at, now()))) * 1000)::bigint,
       updated_at = now()
  FROM biz_flow_instance i
 WHERE n.instance_id = i.id AND n.status IN ('ACTIVE', 'PENDING')
   AND i.status = 'PENDING' AND i.deleted_at IS NULL
   AND i.biz_type = 'CONTRACT_APPROVE';

UPDATE biz_flow_instance
   SET status = 'CANCELLED', ended_at = now(),
       result_comment = COALESCE(result_comment, 'V0209 down：合同审批流回滚'),
       revision = revision + 1, updated_at = now()
 WHERE status = 'PENDING' AND deleted_at IS NULL AND biz_type = 'CONTRACT_APPROVE';

-- ---------- ② 审批中的合同退回草稿（5 值白名单装不下 PENDING_APPROVAL） ----------
UPDATE biz_contract
   SET status = 'DRAFT', approval_instance_id = NULL, updated_at = now()
 WHERE status = 'PENDING_APPROVAL';

-- 已签订 / 已生效行上的实例 id 是审计留痕，回滚后没有列可放 → 一并清空
UPDATE biz_contract SET approval_instance_id = NULL WHERE approval_instance_id IS NOT NULL;

-- ---------- ③ 恢复 5 值白名单 + 去掉审批中约束 ----------
ALTER TABLE biz_contract DROP CONSTRAINT IF EXISTS ck_biz_contract_approval;
ALTER TABLE biz_contract DROP CONSTRAINT IF EXISTS ck_biz_contract_status;
ALTER TABLE biz_contract
    ADD CONSTRAINT ck_biz_contract_status
    CHECK (status IN ('DRAFT', 'SIGNED', 'ACTIVE', 'COMPLETED', 'TERMINATED'));

COMMENT ON COLUMN biz_contract.status IS
    '状态机：DRAFT→SIGNED→ACTIVE→COMPLETED/TERMINATED，另有 SIGNED→TERMINATED；边表在应用层';

-- ---------- ④ 删流程四层（顺序：审批人 → 边 → 节点 → 版本 → 定义） ----------
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node
                    WHERE version_id = '20000000-0000-4000-8000-0000000c020a');
DELETE FROM biz_flow_edge    WHERE version_id = '20000000-0000-4000-8000-0000000c020a';
DELETE FROM biz_flow_node    WHERE version_id = '20000000-0000-4000-8000-0000000c020a';
DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000c020a';
DELETE FROM biz_flow_def     WHERE id = '20000000-0000-4000-8000-0000000c0209';

-- ---------- ⑤ 删列与索引 ----------
DROP INDEX IF EXISTS idx_biz_contract_approval;
ALTER TABLE biz_contract DROP COLUMN IF EXISTS approval_instance_id;

-- ---------- ⑥ 自检：零残留 ----------
DO $$
DECLARE
    n_left int;
BEGIN
    SELECT count(*) INTO n_left FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-0000000c0209';
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0209 down 自检失败：流程定义仍有 % 行残留', n_left;
    END IF;
    SELECT count(*) INTO n_left FROM biz_flow_def_version
     WHERE id = '20000000-0000-4000-8000-0000000c020a';
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0209 down 自检失败：流程版本仍有 % 行残留', n_left;
    END IF;
    SELECT count(*) INTO n_left FROM biz_flow_node
     WHERE version_id = '20000000-0000-4000-8000-0000000c020a';
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0209 down 自检失败：流程节点仍有 % 行残留', n_left;
    END IF;
    SELECT count(*) INTO n_left FROM biz_flow_edge
     WHERE version_id = '20000000-0000-4000-8000-0000000c020a';
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0209 down 自检失败：流程连线仍有 % 行残留', n_left;
    END IF;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_contract' AND column_name = 'approval_instance_id'
    ) THEN
        RAISE EXCEPTION 'V0209 down 自检失败：approval_instance_id 列还在';
    END IF;
    IF pg_get_constraintdef((SELECT oid FROM pg_constraint WHERE conname = 'ck_biz_contract_status'))
       LIKE '%PENDING_APPROVAL%' THEN
        RAISE EXCEPTION 'V0209 down 自检失败：ck_biz_contract_status 还允许 PENDING_APPROVAL';
    END IF;
    RAISE NOTICE 'V0209 down 完成：合同审批流与相关列/约束零残留';
END $$;
