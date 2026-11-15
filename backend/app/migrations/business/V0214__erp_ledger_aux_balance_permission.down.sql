-- V0214 down：撤掉辅助账接口的权限码。
--
-- 顺序：先撤授权（sys_role_menu 有外键指向 sys_menu），再删按钮行。
-- 只删本迁移种的那条 auth_code —— 别的迁移种的行一行不碰。
DELETE FROM sys_role_menu
 WHERE menu_id IN (SELECT id FROM sys_menu
                    WHERE auth_code = 'business:erp:ledger:aux-balance');

DELETE FROM sys_menu
 WHERE auth_code = 'business:erp:ledger:aux-balance';
