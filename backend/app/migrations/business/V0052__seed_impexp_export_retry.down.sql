-- 回滚 V0052：删除 `business:impexp:export:retry` 权限点及其授权
DELETE FROM sys_role_menu
WHERE menu_id = '22000000-0000-0000-0000-000000000114'::uuid;

DELETE FROM sys_menu
WHERE id = '22000000-0000-0000-0000-000000000114'::uuid;
