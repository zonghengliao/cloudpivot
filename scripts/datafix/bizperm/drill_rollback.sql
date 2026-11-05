-- ============================================================================
-- BIZPERM · 生产回滚演练（**事务内**跑 down，测完 ROLLBACK，零净变更）
-- ----------------------------------------------------------------------------
-- 目的：证明「17 分钟能退回原状」，且**不真的把生产退回**（走查窗口内业务不停）。
-- 与隔离副本上的「up→down→up」往返（verify_drill.sh）互补：
--   本文件证明 down 语句在生产数据上能跑通、能精确删干净；
--   副本往返证明来回多次仍然对平。
-- 用法：cd scripts/datafix/bizperm && PGPASSWORD=... psql -d cloudpivot -f drill_rollback.sql
-- ============================================================================
\set ON_ERROR_STOP on
\echo '===== [演练] 回滚前 ====='
SELECT (SELECT count(*) FROM sys_role_menu) AS role_menu_rows,
       (SELECT count(*) FROM sys_menu)     AS menu_rows,
       (SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
         WHERE r.role_code = 'FINANCE_MANAGER') AS fm_rows;

BEGIN;

\i 20_grant.down.sql

\echo '===== [演练] down 之后（事务内未提交）====='
SELECT (SELECT count(*) FROM sys_role_menu) AS role_menu_rows,
       (SELECT count(*) FROM sys_menu)     AS menu_rows,
       (SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
         WHERE r.role_code = 'FINANCE_MANAGER') AS fm_rows;
SELECT r.role_code, count(*) AS granted_rows
  FROM sys_role r JOIN sys_role_menu rm ON rm.role_id = r.id
 WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
 GROUP BY 1 ORDER BY 1;
SELECT count(*) AS voucher_audit_menus_left
  FROM sys_menu WHERE auth_code IN ('business:erp:voucher:audit','business:erp:voucher:post','business:erp:voucher:un-audit');

ROLLBACK;

\echo '===== [演练] ROLLBACK 之后（必须与回滚前完全相同）====='
SELECT (SELECT count(*) FROM sys_role_menu) AS role_menu_rows,
       (SELECT count(*) FROM sys_menu)     AS menu_rows,
       (SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
         WHERE r.role_code = 'FINANCE_MANAGER') AS fm_rows;
