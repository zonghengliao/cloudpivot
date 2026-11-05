\pset border 2
\echo '===== C1 · 逐账号 sys_user_role 前后对照（当前 = 摘权后）====='
SELECT u.username, coalesce(string_agg(r.role_code || '(' || r.data_scope || ')', ', ' ORDER BY r.role_code), '(无角色)') AS roles
  FROM sys_user u LEFT JOIN sys_user_role ur ON ur.user_id = u.id
  LEFT JOIN sys_role r ON r.id = ur.role_id
 WHERE u.username IN ('admin','laoliu','laoliu_test','henry.liao')
 GROUP BY u.username ORDER BY u.username;

\echo '===== C1 · SUPER_ADMIN 成员（目标 = 只有 admin；henry.liao 待老大确认）====='
SELECT u.username, u.status, ur.expire_time
  FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
  JOIN sys_role r ON r.id = ur.role_id
 WHERE r.role_code = 'SUPER_ADMIN' ORDER BY u.username;

\echo '===== C1 · 快照里被摘的 3 行（原样，可回滚）====='
SELECT s.pk, s.row_data->>'user_id' AS user_id, s.row_data->>'role_id' AS role_id,
       s.row_data->>'expire_time' AS expire_time
  FROM datafix_superconv_snapshot s
 WHERE s.batch = 'SUPERCONV-20261105' AND s.tbl = 'sys_user_role' ORDER BY 1;

\echo '===== C3 · OPS_ADMIN 角色定义（remark 必须写明 data_scope=1 的理由）====='
SELECT role_code, role_name, data_scope, status, sort_order, remark FROM sys_role WHERE role_code = 'OPS_ADMIN';

\echo '===== C3 · OPS_ADMIN 权限码（按前缀分布）====='
SELECT split_part(m.auth_code, ':', 1) AS prefix, count(*) AS rows, count(DISTINCT m.auth_code) AS codes
  FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
  JOIN sys_role r ON r.id = rm.role_id
 WHERE r.role_code = 'OPS_ADMIN' AND m.auth_code <> '' GROUP BY 1 ORDER BY 1;

\echo '===== C3 · OPS_ADMIN 是否含业务/ERP/流程码（必须 = 0）====='
SELECT count(*) AS business_rows FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
  JOIN sys_role r ON r.id = rm.role_id
 WHERE r.role_code = 'OPS_ADMIN' AND (m.auth_code LIKE 'business:%' OR m.auth_code LIKE 'erp:%' OR m.auth_code LIKE 'flow:%');

\echo '===== C5 · PENDING 待办对照（摘权不改一行，D13）====='
SELECT t.id, i.instance_no, t.node_key, t.status, u.username AS assignee, t.created_at
  FROM biz_flow_task t JOIN biz_flow_instance i ON i.id = t.instance_id
  LEFT JOIN sys_user u ON u.id = t.assignee_id
 WHERE i.instance_no IN ('FLW-20261102-94417B93','FLW-20261105-5D6B0905')
 ORDER BY i.instance_no, t.created_at, t.node_key;

\echo '===== 不该变的东西（摘权前后必须一致）====='
SELECT 'sys_user 行数            = ' || count(*) FROM sys_user;
SELECT 'sys_user_role 行数       = ' || count(*) FROM sys_user_role;
SELECT 'biz_task.dept_id 非空    = ' || count(*) FROM biz_task WHERE dept_id IS NOT NULL;
SELECT 'biz_project.dept_id 非空 = ' || count(*) FROM biz_project WHERE dept_id IS NOT NULL;
SELECT 'sys_dept.leader_id 非空  = ' || count(*) FROM sys_dept WHERE leader_id IS NOT NULL;
SELECT '账号 status/expire 未动  = ' || count(*) FROM sys_user
 WHERE username IN ('laoliu','laoliu_test') AND status IN (0,1) AND deleted_at IS NULL;
