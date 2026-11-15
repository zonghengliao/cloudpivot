-- V0133 down：撤销「公告与消息」目录，还原 4 个菜单的原归属
-- 原归属：
--   0700 公告中心   → 顶级（parent=NULL, sort=20）
--   0010 消息中心   → 顶级（parent=NULL, sort=10, is_visible=FALSE）
--   0710 公告管理   → content(0790)（sort=71）
--   0410 消息通知   → system(0001)（sort=40）

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000000880';

UPDATE sys_menu SET parent_id = NULL, sort_order = 20, is_visible = TRUE, updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000700';

UPDATE sys_menu SET parent_id = NULL, sort_order = 10, is_visible = FALSE, updated_at = now()
 WHERE id = '23000000-0000-0000-0000-000000000010';

UPDATE sys_menu SET parent_id = '20000000-0000-0000-0000-000000000790', sort_order = 71, updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000710';

UPDATE sys_menu SET parent_id = '10000000-0000-0000-0000-000000000001', sort_order = 40, updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000410';

DELETE FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000000880';
