\pset border 1
\echo '################ A) 3 页 x 3 角色 mode=READ —— 真实展开结果（复用 expand.sql 原文，md5 见 02 文件）################'
BEGIN;
\i /tmp/impexp_probe/probe_plan_read.sql
\i /tmp/impexp_probe/expand.sql
\echo '-- A1) 每个角色最终会被授予的 impexp 权限码（_sel ∩ sys_menu）'
SELECT s.role_code, m.auth_code,
       substring(m.auth_code from '([^:]+)$') AS action
  FROM _sel s JOIN sys_menu m ON m.id = s.id
 WHERE m.auth_code LIKE 'business:impexp:%'
 ORDER BY 1,2;
\echo '-- A2) 断言：READ 展开里绝不能出现 export:create / import:confirm（出现即为外带/写入入口）'
SELECT count(*) FILTER (WHERE m.auth_code = 'business:impexp:export:create')  AS has_export_create,
       count(*) FILTER (WHERE m.auth_code = 'business:impexp:import:confirm') AS has_import_confirm,
       count(*) FILTER (WHERE m.auth_code = 'business:impexp:export:delete')  AS has_export_delete,
       count(*) FILTER (WHERE m.auth_code = 'business:impexp:import:retry')   AS has_import_retry
  FROM _sel s JOIN sys_menu m ON m.id = s.id;
\echo '-- A3) 每个角色被授的 impexp 码个数'
SELECT role_code, count(*) AS impexp_codes
  FROM _sel s JOIN sys_menu m ON m.id = s.id
 WHERE m.auth_code LIKE 'business:impexp:%' GROUP BY 1 ORDER BY 1;
ROLLBACK;

\echo ''
\echo '################ B) 对照：同样 3 页 mode=FULL —— 展开结果 ################'
BEGIN;
\i /tmp/impexp_probe/probe_plan_full.sql
\i /tmp/impexp_probe/expand.sql
SELECT s.role_code, m.auth_code, substring(m.auth_code from '([^:]+)$') AS action
  FROM _sel s JOIN sys_menu m ON m.id = s.id
 WHERE m.auth_code LIKE 'business:impexp:%'
 ORDER BY 1,2;
\echo '-- B1) FULL 展开里是否出现外带/写入入口（这就是"能办"的代价）'
SELECT count(*) FILTER (WHERE m.auth_code = 'business:impexp:export:create')  AS has_export_create,
       count(*) FILTER (WHERE m.auth_code = 'business:impexp:import:confirm') AS has_import_confirm
  FROM _sel s JOIN sys_menu m ON m.id = s.id;
ROLLBACK;
