-- V0134 down：撤销任务模板（菜单 → 授权 → 表）
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000000902';

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000000902';

DROP TABLE IF EXISTS biz_task_template;
