-- 回滚 V0207：只删「结账催办」权限节点与其授权行。
-- 已发出的催办通知（sys_notice 里 biz_type = 'period_remind' 的行）属于沟通/审计记录，一行不动。
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001212'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001212'::uuid
   AND auth_code = 'business:erp:period:remind';
