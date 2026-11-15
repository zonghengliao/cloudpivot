-- 回滚 V0194：删掉流程模板库菜单节点（按钮节点本迁移未建，故只删页面节点）。
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-0000000007a5'::uuid;
DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-0000000007a5'::uuid;
