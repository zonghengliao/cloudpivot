-- ============================================================================
-- 断言（必须每行 res = PASS）
--   用法：psql -v exp=4 -f 30_verify.sql     （up 之后 exp=4；down 之后 exp=1）
--   设计：用 SELECT 产出 PASS/FAIL 行，由 drill.sh 统一 grep "FAIL" 判红 ——
--         比 DO $$ RAISE EXCEPTION 更透明（失败时看得到是哪条、差多少）。
-- ============================================================================
\pset pager off
\pset footer off

\echo '--- A. 4 个 QA 夹具角色的 data_scope 目标值 ---'
SELECT 'A1 夹具角色 data_scope 全为预期值(psql -v exp=…)' AS 断言,
       CASE WHEN count(*) FILTER (WHERE data_scope <> :exp::int) = 0
             AND count(*) = 4 THEN 'PASS' ELSE 'FAIL' END AS res,
       string_agg(role_code || '=' || data_scope::text, ', ' ORDER BY role_code) AS 实际
  FROM sys_role
 WHERE deleted_at IS NULL
   AND role_code IN ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B');

\echo '--- B. 它们仍然 0 个活跃持有者（收紧的前提没变） ---'
SELECT 'B1 夹具角色活跃持有者 = 0' AS 断言,
       CASE WHEN coalesce(sum(h.n), 0) = 0 THEN 'PASS' ELSE 'FAIL' END AS res,
       string_agg(r.role_code || ':' || coalesce(h.n, 0)::text, ', ' ORDER BY r.role_code) AS 实际
  FROM sys_role r
  LEFT JOIN LATERAL (
       SELECT count(*) AS n
         FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id
        WHERE ur.role_id = r.id AND u.deleted_at IS NULL AND u.status = 1) h ON true
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B');

\echo '--- C. 角色行本身没被动（未软删 / 状态仍是 1 / 权限码条数不变） ---'
SELECT 'C1 夹具角色行仍在且 status=1' AS 断言,
       CASE WHEN count(*) = 4 AND count(*) FILTER (WHERE status = 1) = 4 THEN 'PASS' ELSE 'FAIL' END AS res,
       count(*)::text || ' 行' AS 实际
  FROM sys_role
 WHERE deleted_at IS NULL
   AND role_code IN ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B');

SELECT 'C2 夹具角色权限码条数' AS 断言,
       'INFO' AS res,
       coalesce(string_agg(role_code || ':' || n::text, ', ' ORDER BY role_code), '无') AS 实际
  FROM (SELECT r.role_code, (SELECT count(*) FROM sys_role_menu rm WHERE rm.role_id = r.id) AS n
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B')) t;

\echo '--- D. 不许误伤：业务角色范围（V0247 口径）与超管 ---'
SELECT 'D1 业务角色范围保持 V0247 口径' AS 断言,
       CASE WHEN count(*) FILTER (WHERE role_code IN ('DEPT_LEADER','PURCHASE_MANAGER','WAREHOUSE_MANAGER') AND data_scope <> 3) = 0
             AND count(*) FILTER (WHERE role_code IN ('FINANCE_MANAGER','FINANCE_DIRECTOR') AND data_scope <> 5) = 0
             AND count(*) FILTER (WHERE role_code = 'GENERAL_MANAGER' AND data_scope <> 1) = 0
            THEN 'PASS' ELSE 'FAIL' END AS res,
       string_agg(role_code || '=' || data_scope::text, ', ' ORDER BY role_code) AS 实际
  FROM sys_role
 WHERE deleted_at IS NULL
   AND role_code IN ('DEPT_LEADER','PURCHASE_MANAGER','WAREHOUSE_MANAGER',
                     'FINANCE_MANAGER','FINANCE_DIRECTOR','GENERAL_MANAGER');

SELECT 'D2 SUPER_ADMIN 仍为 1（超管不许被限死）' AS 断言,
       CASE WHEN count(*) = 1 AND count(*) FILTER (WHERE data_scope = 1) = 1 THEN 'PASS' ELSE 'FAIL' END AS res,
       coalesce(string_agg(data_scope::text, ','), '缺失') AS 实际
  FROM sys_role WHERE deleted_at IS NULL AND role_code = 'SUPER_ADMIN';

\echo '--- E. 巡检 N1 的两条判据（本迁移不许把 N1 弄红） ---'
SELECT 'E1 N1-a 至少一个启用角色是部门级范围' AS 断言,
       CASE WHEN count(*) > 0 THEN 'PASS' ELSE 'FAIL' END AS res,
       coalesce(string_agg(role_code || '(' || data_scope::text || ')', ', '), '无') AS 实际
  FROM sys_role r
 WHERE r.deleted_at IS NULL AND r.status = 1
   AND (r.data_scope IN (2,3)
        OR (r.data_scope = 5 AND EXISTS (SELECT 1 FROM sys_role_dept rd WHERE rd.role_id = r.id)));

SELECT 'E2 N1-b 无「scope=5 但集合为空」的角色' AS 断言,
       CASE WHEN count(*) = 0 THEN 'PASS' ELSE 'FAIL' END AS res,
       coalesce(string_agg(role_code, ','), '无') AS 实际
  FROM sys_role r
 WHERE r.deleted_at IS NULL AND r.data_scope = 5
   AND NOT EXISTS (SELECT 1 FROM sys_role_dept rd WHERE rd.role_id = r.id);

\echo '--- F. sys_role_dept 行数（本迁移不碰集合） ---'
SELECT 'F1 sys_role_dept 行数' AS 断言,
       'INFO' AS res,
       count(*)::text AS 实际
  FROM sys_role_dept;

\echo '--- G. 逐账号核算：收紧后谁还能看到「全部数据」 ---'
SELECT 'G1 ' || u.username AS 断言,
       CASE WHEN coalesce(bool_or(r.role_code = 'SUPER_ADMIN'), false) THEN 'INFO'
            WHEN count(r.id) = 0 THEN 'INFO'
            ELSE 'INFO' END AS res,
       '启用角色=' || coalesce(string_agg(r.role_code || '(' || r.data_scope::text || ')', ',' ORDER BY r.role_code), '无')
         || ' ⇒ ' ||
         CASE WHEN coalesce(bool_or(r.role_code = 'SUPER_ADMIN'), false) THEN 'ALL（超管短路）'
              WHEN count(r.id) = 0 THEN 'SELF（无角色=仅本人）'
              WHEN bool_or(r.data_scope = 1) THEN 'ALL（仍有 scope=1 的角色）'
              WHEN bool_or(r.data_scope = 2) THEN 'DEPT'
              WHEN bool_or(r.data_scope = 3) THEN 'DEPT_AND_CHILD'
              WHEN bool_or(r.data_scope = 4) THEN 'SELF'
              ELSE 'CUSTOM'
         END AS 实际
  FROM sys_user u
  LEFT JOIN sys_user_role ur ON ur.user_id = u.id AND (ur.expire_time IS NULL OR ur.expire_time > now())
  LEFT JOIN sys_role r ON r.id = ur.role_id AND r.deleted_at IS NULL AND r.status = 1
 WHERE u.deleted_at IS NULL AND u.status = 1
 GROUP BY u.username
 ORDER BY u.username;
