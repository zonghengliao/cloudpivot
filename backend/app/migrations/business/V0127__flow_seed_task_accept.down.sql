-- ======================================================
-- V0127 down：删除种子流程 task_accept（固定 UUID 那一套）
-- 只删 `id`/`flow_code` 双条件命中的行：客户自己建的 task_accept（其它 UUID）不动。
-- 子表靠 ON DELETE CASCADE（node/edge/assignee 指向 version / node），版本行显式删。
-- ======================================================
DELETE FROM biz_flow_edge WHERE version_id = '20000000-0000-4000-8000-00000000e127';
DELETE FROM biz_flow_node_assignee WHERE node_id IN (
    SELECT id FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-00000000e127');
DELETE FROM biz_flow_node WHERE version_id = '20000000-0000-4000-8000-00000000e127';
DELETE FROM biz_flow_def_version WHERE id = '20000000-0000-4000-8000-00000000e127';

-- 先降级再摘版本：ck_biz_flow_def_published_has_version 禁止「PUBLISHED 但没有版本」
UPDATE biz_flow_def SET status = 'DISABLED', current_version_id = NULL
 WHERE id = '20000000-0000-4000-8000-00000000d127'
   AND current_version_id = '20000000-0000-4000-8000-00000000e127';
DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-00000000d127' AND flow_code = 'task_accept';

DO $$
DECLARE
    left_rows int;
BEGIN
    SELECT count(*) INTO left_rows FROM biz_flow_def
     WHERE id = '20000000-0000-4000-8000-00000000d127';
    IF left_rows <> 0 THEN
        RAISE EXCEPTION 'V0127 down 失败：定义行残留 %', left_rows;
    END IF;
    SELECT count(*) INTO left_rows FROM biz_flow_def_version
     WHERE id = '20000000-0000-4000-8000-00000000e127';
    IF left_rows <> 0 THEN
        RAISE EXCEPTION 'V0127 down 失败：版本行残留 %', left_rows;
    END IF;
    SELECT count(*) INTO left_rows FROM biz_flow_node
     WHERE version_id = '20000000-0000-4000-8000-00000000e127';
    IF left_rows <> 0 THEN
        RAISE EXCEPTION 'V0127 down 失败：节点行残留 %', left_rows;
    END IF;
    SELECT count(*) INTO left_rows FROM biz_flow_edge
     WHERE version_id = '20000000-0000-4000-8000-00000000e127';
    IF left_rows <> 0 THEN
        RAISE EXCEPTION 'V0127 down 失败：边行残留 %', left_rows;
    END IF;
END $$;
