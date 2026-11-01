-- ======================================================
-- V0186 回滚：删掉本次补的 12 个外键索引
--
-- 只删索引，不删表、不动数据、不动任何其它索引（含 PK/唯一键/既有业务索引）。
-- 全部 `DROP INDEX IF EXISTS` → 可重复执行。
-- ======================================================

DROP INDEX IF EXISTS idx_biz_task_worklog_user_id;

DROP INDEX IF EXISTS idx_biz_task_template_project_id;
DROP INDEX IF EXISTS idx_biz_task_template_owner_id;

DROP INDEX IF EXISTS idx_biz_task_comment_user_id;
DROP INDEX IF EXISTS idx_biz_task_comment_parent_comment_id;

DROP INDEX IF EXISTS idx_biz_stock_transfer_item_to_location_id;
DROP INDEX IF EXISTS idx_biz_stock_transfer_item_from_location_id;
DROP INDEX IF EXISTS idx_biz_stock_transfer_transit_warehouse_id;

DROP INDEX IF EXISTS idx_biz_flow_task_transferred_from;
DROP INDEX IF EXISTS idx_biz_flow_instance_node_node_id;
DROP INDEX IF EXISTS idx_biz_flow_instance_version_id;
DROP INDEX IF EXISTS idx_biz_flow_instance_prev_instance;
