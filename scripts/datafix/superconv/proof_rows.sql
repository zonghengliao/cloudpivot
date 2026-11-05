-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 留档用只读核对（施工前后各跑一次，diff 即证据）
-- 用法：psql ... -f proof_rows.sql | tee 证据文件
-- ============================================================================
\pset border 2
\echo '===== C1 · SUPER_ADMIN 成员（目标 = 只有 admin）====='
SELECT u.username, u.status, ur.expire_time
  FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
  JOIN sys_role r ON r.id = ur.role_id
 WHERE r.role_code = 'SUPER_ADMIN' ORDER BY u.username;

\echo '===== C1 · 逐账号角色（施工前后对照）====='
SELECT u.username,
       coalesce(d.dept_code, 'NULL') AS dept,
       u.status,
       coalesce(string_agg(r.role_code || '(' || r.data_scope || ')', ', ' ORDER BY r.role_code), '(无角色)') AS roles
  FROM sys_user u
  LEFT JOIN sys_dept d ON d.id = u.dept_id
  LEFT JOIN sys_user_role ur ON ur.user_id = u.id
  LEFT JOIN sys_role r ON r.id = ur.role_id
 WHERE u.username IN ('admin','laoliu','laoliu_test','henry.liao')
 GROUP BY u.username, d.dept_code, u.status ORDER BY u.username;

\echo '===== 快照（batch → 行数；down 靠它精确回退）====='
SELECT to_regclass('public.datafix_superconv_snapshot') IS NOT NULL AS has_snap \gset
\if :has_snap
SELECT batch, tbl, count(*) AS rows FROM datafix_superconv_snapshot GROUP BY batch, tbl ORDER BY batch, tbl;

\echo '===== 阶段 2 快照内容（4 行绑定）====='
SELECT s.pk,
       (s.row_data->>'user_id')::uuid AS user_id,
       (s.row_data->>'role_id')::uuid AS role_id,
       s.row_data->>'expire_time' AS expire_time
  FROM datafix_superconv_snapshot s
 WHERE s.batch = 'SUPERCONV-20261105' AND s.tbl = 'sys_user_role' ORDER BY 1;
\else
\echo '（快照表还没建 —— 本库还没跑过 apply，这是「施工前」的正常状态）'
\endif

\echo '===== 业务角色缺祖先目录（处，目标 = 0）====='
WITH RECURSIVE tr AS (
  SELECT id FROM sys_role
   WHERE role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
     AND deleted_at IS NULL
), seed AS (
  SELECT rm.role_id, m.id AS menu_id, m.parent_id
    FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
   WHERE rm.role_id IN (SELECT id FROM tr) AND m.deleted_at IS NULL AND m.menu_type = 2
), up AS (
  SELECT s.role_id, s.parent_id AS node_id FROM seed s WHERE s.parent_id IS NOT NULL
  UNION
  SELECT u.role_id, m.parent_id FROM up u JOIN sys_menu m ON m.id = u.node_id
   WHERE m.parent_id IS NOT NULL AND m.deleted_at IS NULL
)
SELECT count(*) AS orphan_dirs FROM up u
  JOIN sys_menu d ON d.id = u.node_id AND d.deleted_at IS NULL AND d.status = 1
 WHERE d.menu_type = 1
   AND NOT EXISTS (SELECT 1 FROM sys_role_menu x WHERE x.role_id = u.role_id AND x.menu_id = u.node_id);

\echo '===== 各账号可见业务页（与接口 /system/auth/menus 同口径：menu_type=2 且已授权）====='
WITH r AS (
  SELECT u.username, rm.menu_id
    FROM sys_user u JOIN sys_user_role ur ON ur.user_id = u.id
    JOIN sys_role_menu rm ON rm.role_id = ur.role_id
   WHERE u.username IN ('laoliu','laoliu_test','henry.liao')
     AND (ur.expire_time IS NULL OR ur.expire_time > now())
), m AS (
  SELECT DISTINCT r.username, s.id, s.path, s.auth_code
    FROM r JOIN sys_menu s ON s.id = r.menu_id
   WHERE s.deleted_at IS NULL AND s.status = 1 AND s.menu_type = 2
)
SELECT username, count(*) AS pages,
       coalesce(string_agg(coalesce(path, auth_code), ', ' ORDER BY coalesce(path, auth_code)), '(无)') AS page_list
  FROM m GROUP BY username ORDER BY username;

\echo '===== C5 · D13：2 张 PENDING 实例的待办（施工前后必须逐字一致）====='
SELECT t.id, i.instance_no, t.node_key, t.status, u.username AS assignee, t.created_at
  FROM biz_flow_task t JOIN biz_flow_instance i ON i.id = t.instance_id
  LEFT JOIN sys_user u ON u.id = t.assignee_id
 WHERE i.instance_no IN ('FLW-20261102-94417B93','FLW-20261105-5D6B0905')
 ORDER BY i.instance_no, t.created_at, t.node_key;

\echo '===== 不该变的计数（施工前后必须完全相同）====='
SELECT 'sys_user 行数               = ' || count(*) FROM sys_user;
SELECT 'sys_user_role 行数          = ' || count(*) FROM sys_user_role;
SELECT 'biz_task.dept_id 非空       = ' || count(*) FROM biz_task WHERE dept_id IS NOT NULL;
SELECT 'biz_project.dept_id 非空    = ' || count(*) FROM biz_project WHERE dept_id IS NOT NULL;
SELECT 'sys_dept.leader_id 非空     = ' || count(*) FROM sys_dept WHERE leader_id IS NOT NULL;
SELECT 'biz_flow_task PENDING       = ' || count(*) FROM biz_flow_task WHERE status = 'PENDING';
SELECT 'status/expire 未被停用      = ' || count(*) FROM sys_user
 WHERE username IN ('laoliu','laoliu_test','henry.liao') AND status IN (0,1) AND deleted_at IS NULL;
