-- V0255 回滚：撤掉应付账龄分析的菜单与授权（不涉及任何业务表 / 数据）。
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001600'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001600'::uuid;
