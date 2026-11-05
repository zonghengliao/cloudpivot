-- ============================================================================
-- 快照（kv 口径）：key|value，一行一项；两次快照整串相等才算「没动别的东西」
--   用法：psql -At -F'|' -f 20_snapshot.sql > snap.txt
-- ============================================================================
SELECT 'role:' || role_code, data_scope::text
  FROM sys_role WHERE deleted_at IS NULL ORDER BY role_code;

SELECT 'role_status:' || role_code, status::text
  FROM sys_role WHERE deleted_at IS NULL ORDER BY role_code;

SELECT 'role_softdeleted_count', count(*)::text
  FROM sys_role WHERE deleted_at IS NOT NULL;

SELECT 'table_count:sys_role',            count(*)::text FROM sys_role
UNION ALL SELECT 'table_count:sys_role_dept',    count(*)::text FROM sys_role_dept
UNION ALL SELECT 'table_count:sys_user',         count(*)::text FROM sys_user
UNION ALL SELECT 'table_count:sys_user_role',    count(*)::text FROM sys_user_role
UNION ALL SELECT 'table_count:sys_role_menu',    count(*)::text FROM sys_role_menu
UNION ALL SELECT 'table_count:sys_role_mutex',   count(*)::text FROM sys_role_mutex
UNION ALL SELECT 'table_count:sys_dept',         count(*)::text FROM sys_dept
ORDER BY 1;

SELECT 'user:' || username, status::text
  FROM sys_user WHERE deleted_at IS NULL ORDER BY username;

-- 每个账号的启用角色集合（收紧范围不该改绑定关系）
SELECT 'bind:' || u.username,
       coalesce(string_agg(r.role_code, ',' ORDER BY r.role_code), '(none)')
  FROM sys_user u
  LEFT JOIN sys_user_role ur ON ur.user_id = u.id
  LEFT JOIN sys_role r ON r.id = ur.role_id AND r.deleted_at IS NULL
 WHERE u.deleted_at IS NULL
 GROUP BY u.username ORDER BY u.username;
