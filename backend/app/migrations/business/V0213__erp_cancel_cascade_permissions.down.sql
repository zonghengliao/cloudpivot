-- V0213 down：撤掉联动作废的两个权限码。
--
-- 顺序：先撤授权（sys_role_menu 有外键指向 sys_menu），再删按钮行。
-- 只删本迁移种的两条 auth_code —— 别的迁移种的行一行不碰。
DELETE FROM sys_role_menu
 WHERE menu_id IN (SELECT id FROM sys_menu
                    WHERE auth_code IN ('business:erp:doc:cancel-impact',
                                        'business:erp:doc:cancel-cascade'));

DELETE FROM sys_menu
 WHERE auth_code IN ('business:erp:doc:cancel-impact',
                     'business:erp:doc:cancel-cascade');
