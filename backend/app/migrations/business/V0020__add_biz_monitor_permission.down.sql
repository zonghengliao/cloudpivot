-- 回滚 V0020：移除服务器状态权限点及其角色授权。
-- 先删授权行（外键指向 sys_menu），再删菜单行。

DELETE FROM sys_role_menu
WHERE menu_id = '20000000-0000-0000-0000-000000000001'::uuid;

DELETE FROM sys_menu
WHERE id = '20000000-0000-0000-0000-000000000001'::uuid;
