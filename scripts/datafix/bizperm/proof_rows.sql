-- ============================================================================
-- BIZPERM · 对平巡检（施工前后同一份 SQL，输出直接贴证据）
-- 用法：psql -d <db> -f proof_rows.sql
-- ============================================================================
\pset border 2
\pset pager off

\echo '===== 1) 目标态不变量：SUPER_ADMIN 成员（必须 = 1 且只有 admin）====='
SELECT u.username, u.status FROM sys_user_role ur
  JOIN sys_user u ON u.id = ur.user_id JOIN sys_role r ON r.id = ur.role_id
 WHERE r.role_code = 'SUPER_ADMIN' ORDER BY 1;

\echo '===== 2) 5 个业务角色：data_scope（本卡不动）/ 授权行数 ====='
SELECT r.role_code, r.data_scope,
       count(*) FILTER (WHERE m.menu_type = 1) AS dirs,
       count(*) FILTER (WHERE m.menu_type = 2) AS pages,
       count(*) FILTER (WHERE m.menu_type = 3) AS buttons,
       count(*) AS total
  FROM sys_role r LEFT JOIN sys_role_menu rm ON rm.role_id = r.id
  LEFT JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
 WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
 GROUP BY 1,2 ORDER BY 1;

\echo '===== 3) 并集覆盖的业务页数（分母 = 全平台 menu_type=2 + module=business）====='
WITH biz AS (SELECT id FROM sys_menu WHERE menu_type = 2 AND deleted_at IS NULL AND module_code = 'business'),
     g AS (SELECT DISTINCT rm.menu_id FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
            WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER'))
SELECT (SELECT count(*) FROM biz) AS business_pages_total,
       (SELECT count(*) FROM biz JOIN g ON g.menu_id = biz.id) AS covered_by_5_roles,
       (SELECT count(*) FROM biz WHERE id NOT IN (SELECT menu_id FROM g)) AS still_uncovered;

\echo '===== 4) 仍然未覆盖的业务页明细（应当只剩平台治理 + 导入导出）====='
WITH biz AS (SELECT id, path, auth_code, name FROM sys_menu WHERE menu_type = 2 AND deleted_at IS NULL AND module_code = 'business'),
     g AS (SELECT DISTINCT rm.menu_id FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
            WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER'))
SELECT path, auth_code FROM biz WHERE id NOT IN (SELECT menu_id FROM g) ORDER BY path;

\echo '===== 5) 逐角色可见业务页（供对照 /system/auth/menus）====='
SELECT r.role_code, count(*) AS pages,
       string_agg(m.path, ', ' ORDER BY m.path) AS page_list
  FROM sys_role r JOIN sys_role_menu rm ON rm.role_id = r.id
  JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL AND m.menu_type = 2
 WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
 GROUP BY 1 ORDER BY 1;

\echo '===== 6) 平台治理页：任何业务角色持有 = 0（红线检查）====='
SELECT count(*) AS governance_leaks
  FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
  JOIN sys_menu m ON m.id = rm.menu_id
 WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
   AND m.path IN ('/system/user','/system/role','/system/dept','/system/menu','/system/data-scope',
                  '/system/field-mask','/system/role-mutex','/system/change-log',
                  '/business/storage/backend','/support/config','/support/dict','/support/file',
                  '/support/job','/support/log','/support/online');

\echo '===== 7) 祖先目录孤儿检查：业务角色的页面若父目录未授权 → 侧边栏会丢页（目标 0）====='
SELECT count(*) AS orphan_pages FROM (
  SELECT rm.role_id, m.id
    FROM sys_role_menu rm
    JOIN sys_role r ON r.id = rm.role_id
    JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL AND m.menu_type = 2
   WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
     AND m.parent_id IS NOT NULL
     AND NOT EXISTS (SELECT 1 FROM sys_role_menu p WHERE p.role_id = rm.role_id AND p.menu_id = m.parent_id
                        AND EXISTS (SELECT 1 FROM sys_menu d WHERE d.id = p.menu_id AND d.status = 1))
) t;

\echo '===== 8) 记账凭证 审核/记账/反审核 三条码：存在 + 只授给财务经理 ====='
SELECT m.auth_code, string_agg(r.role_code, ',' ORDER BY r.role_code) AS granted_to
  FROM sys_menu m LEFT JOIN sys_role_menu rm ON rm.menu_id = m.id
  LEFT JOIN sys_role r ON r.id = rm.role_id
 WHERE m.auth_code IN ('business:erp:voucher:audit','business:erp:voucher:post','business:erp:voucher:un-audit')
 GROUP BY 1 ORDER BY 1;

\echo '===== 9) 账号 → 角色 / 部门 / token_version ====='
SELECT u.username, d.dept_code, u.status, u.token_version,
       string_agg(r.role_code, ',' ORDER BY r.role_code) AS roles
  FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id
  LEFT JOIN sys_user_role ur ON ur.user_id = u.id
  LEFT JOIN sys_role r ON r.id = ur.role_id
 WHERE u.username IN ('admin','laoliu','henry.liao','laoliu_test')
 GROUP BY 1,2,3,4 ORDER BY 1;

\echo '===== 10) 不该变的计数（施工前后必须完全相同）====='
SELECT 'sys_user 行数            = ' || count(*) FROM sys_user
UNION ALL SELECT 'sys_user_role 行数       = ' || count(*) FROM sys_user_role
UNION ALL SELECT 'sys_menu 行数            = ' || count(*) FROM sys_menu WHERE deleted_at IS NULL
UNION ALL SELECT 'biz_task 行数            = ' || count(*) FROM biz_task
UNION ALL SELECT 'biz_task.dept_id 非空    = ' || count(*) FROM biz_task WHERE dept_id IS NOT NULL
UNION ALL SELECT 'biz_project.dept_id 非空 = ' || count(*) FROM biz_project WHERE dept_id IS NOT NULL
UNION ALL SELECT 'biz_flow_task PENDING    = ' || count(*) FROM biz_flow_task WHERE status = 'PENDING';
