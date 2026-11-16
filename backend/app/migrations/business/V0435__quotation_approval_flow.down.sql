-- V0435 回滚：删除报价单审批流程种子 + 报价单的审批实例列。
--
-- 顺序：先删实例/待办里**本流程**的行（否则 `biz_flow_task` / `biz_flow_instance` 的外键
-- 会挡住 node/edge 的删除），再删图 → 版本 → 定义，最后删列。
--
-- 注意：**不动 `biz_quotation.status`**（审批不是状态，本迁移从未改过它）。
-- 若库里已经有「审批通过 → 自动发出」的历史单据，回滚本迁移**不会**把它们打回草稿：
-- 发出是既成事实（客户手里有报价），回滚迁移不负责编造历史。

DELETE FROM biz_flow_task
 WHERE instance_id IN (
     SELECT id FROM biz_flow_instance WHERE biz_type = 'QUOTATION_APPROVE'
 );

DELETE FROM biz_flow_instance WHERE biz_type = 'QUOTATION_APPROVE';

UPDATE biz_quotation SET approval_instance_id = NULL
 WHERE approval_instance_id IS NOT NULL;

DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (
     SELECT id FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0432'::uuid
 );

DELETE FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-0000000d0432'::uuid;
DELETE FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-0000000d0432'::uuid;
DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-0000000d0432'::uuid;
DELETE FROM biz_flow_def WHERE id = '20000000-0000-4000-8000-0000000d0431'::uuid;

DROP INDEX IF EXISTS idx_biz_quotation_approval;
ALTER TABLE biz_quotation DROP COLUMN IF EXISTS approval_instance_id;
