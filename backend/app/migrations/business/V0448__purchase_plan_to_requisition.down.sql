-- ============================================================================
-- V0448 down：回滚「采购计划 → 采购申请」的溯源列与按钮
--
--   顺序：授权 → 菜单 → 索引 → 列（列最后删，删列会连带删掉依赖它的索引，
--   所以索引那句只是让「列还在、索引先没了」的中间态也能安全重跑）。
--   幂等：全部 IF EXISTS / DELETE 按 id。
-- ============================================================================

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000004477'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000004477'::uuid;

DROP INDEX IF EXISTS idx_biz_prqi_plan_item;
DROP INDEX IF EXISTS idx_biz_purchase_requisition_source_plan;

ALTER TABLE biz_purchase_requisition_item
    DROP COLUMN IF EXISTS plan_item_id;

ALTER TABLE biz_purchase_requisition
    DROP COLUMN IF EXISTS source_plan_id;
