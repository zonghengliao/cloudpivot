-- 回滚 V0208：只删「年结催办」权限节点与其授权行。
-- 已发出的年结催办通知（sys_notice 里 biz_type = 'year_end_remind' 的行）属于沟通/审计记录，一行不动。
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001213'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001213'::uuid
   AND auth_code = 'business:erp:year-end:remind';
