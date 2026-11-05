\pset border 1
\timing off
\echo '== 1) 三个导入导出页节点（menu_type=2）=='
SELECT m.id, m.path, m.menu_type, m.auth_code, m.name
  FROM sys_menu m
 WHERE m.path IN ('/business/impexp/import','/business/impexp/import-task','/business/impexp/export-task')
   AND m.deleted_at IS NULL
 ORDER BY 2;

\echo ''
\echo '== 2) 三个页面的全部后代（按钮码），按 auth_code 末段动作排序 =='
WITH RECURSIVE roots AS (
  SELECT id, path FROM sys_menu
   WHERE path IN ('/business/impexp/import','/business/impexp/import-task','/business/impexp/export-task')
     AND menu_type = 2 AND deleted_at IS NULL
), d AS (
  SELECT r.path AS page, m.id, m.auth_code, m.menu_type, m.name, 1 AS depth
    FROM roots r JOIN sys_menu m ON m.parent_id = r.id AND m.deleted_at IS NULL
  UNION ALL
  SELECT d.page, m.id, m.auth_code, m.menu_type, m.name, d.depth + 1
    FROM d JOIN sys_menu m ON m.parent_id = d.id AND m.deleted_at IS NULL
)
SELECT page, auth_code, menu_type, name, depth,
       substring(auth_code from '([^:]+)$') AS action
  FROM d ORDER BY page, auth_code;

\echo ''
\echo '== 3) 现状：哪些角色已经持有这些码（应为：无业务角色）=='
SELECT r.role_code, m.auth_code
  FROM sys_role_menu rm
  JOIN sys_role r ON r.id = rm.role_id
  JOIN sys_menu m ON m.id = rm.menu_id
 WHERE m.auth_code LIKE 'business:impexp:%'
 ORDER BY 1,2;

\echo ''
\echo '== 4) 三张 ERP 单据表的归属列（dept_id 是否存在）=='
SELECT table_name, column_name
  FROM information_schema.columns
 WHERE table_schema='public'
   AND table_name IN ('biz_stock_in','biz_payable','biz_voucher')
   AND column_name IN ('dept_id','create_by','created_by','owner_dept_id','aux_dept_id')
 ORDER BY 1,2;

\echo ''
\echo '== 5) 行数（导出内容查询的"全量"基准）=='
SELECT 'biz_stock_in' AS t, count(*) AS rows FROM biz_stock_in WHERE deleted_at IS NULL
UNION ALL SELECT 'biz_payable', count(*) FROM biz_payable WHERE deleted_at IS NULL
UNION ALL SELECT 'biz_voucher', count(*) FROM biz_voucher WHERE deleted_at IS NULL;
