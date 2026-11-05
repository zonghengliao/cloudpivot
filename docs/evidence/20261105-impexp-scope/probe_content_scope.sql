\pset border 1
\echo '################ C) 导出内容查询 vs 列表查询：同一张表、同一个口径（均在隔离副本 cloudpivot_impexp_iso1 上只读）################'
\echo '-- C1) 导出定义 erp.stock_in 的 COUNT(*)（SQL 原文取自 backend/src/business/impexp/builtin/erp_doc.rs:466 的 format!("SELECT COUNT(*) {}", self.spec.from) + :395 的 from）'
SELECT COUNT(*) AS export_count
  FROM biz_stock_in s
  LEFT JOIN biz_supplier sp ON sp.id = s.supplier_id AND sp.deleted_at IS NULL
  LEFT JOIN biz_warehouse w ON w.id = s.warehouse_id AND w.deleted_at IS NULL
 WHERE s.deleted_at IS NULL;

\echo '-- C2) 收货入库单列表口径（stockin_base.rs:632 LIST_TARGET = unscoped("biz_stock_in")；paginate_unscoped 注入的是 AND 1 = 1）'
SELECT COUNT(*) AS list_count FROM biz_stock_in s WHERE s.deleted_at IS NULL AND 1 = 1;

\echo '-- C3) 应付单：导出（erp_doc.rs:414 from）vs 列表（payable_query.rs:100 LIST_TARGET unscoped）'
SELECT (SELECT COUNT(*) FROM biz_payable p
          LEFT JOIN biz_supplier sp ON sp.id = p.supplier_id AND sp.deleted_at IS NULL
         WHERE p.deleted_at IS NULL) AS export_count,
       (SELECT COUNT(*) FROM biz_payable p WHERE p.deleted_at IS NULL AND 1 = 1) AS list_count;

\echo '-- C4) 凭证：导出（erp_doc.rs:433 from）vs 列表（voucher_query.rs:102 LIST_TARGET unscoped）'
SELECT (SELECT COUNT(*) FROM biz_voucher v WHERE v.deleted_at IS NULL) AS export_count,
       (SELECT COUNT(*) FROM biz_voucher v WHERE v.deleted_at IS NULL AND 1 = 1) AS list_count;

\echo ''
\echo '-- C5) 这些单据到底归属几个部门？（若导出真按部门过滤，条数应远小于 C1/C3/C4）'
SELECT 'biz_stock_in' AS t, count(DISTINCT d.dept_name) AS depts, count(*) AS rows
  FROM biz_stock_in s LEFT JOIN sys_user u ON u.id = s.create_by LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE s.deleted_at IS NULL
UNION ALL
SELECT 'biz_payable', count(DISTINCT d.dept_name), count(*)
  FROM biz_payable p LEFT JOIN sys_user u ON u.id = p.create_by LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE p.deleted_at IS NULL
UNION ALL
SELECT 'biz_voucher', count(DISTINCT d.dept_name), count(*)
  FROM biz_voucher v LEFT JOIN sys_user u ON u.id = v.create_by LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE v.deleted_at IS NULL;

\echo ''
\echo '-- C6) 逐部门分布（含 NULL = 归属人不在 sys_user 或未落部门）：导出**不做**这个切分'
SELECT COALESCE(d.dept_name, '(无部门)') AS dept, 'stock_in' AS t, count(*) AS rows
  FROM biz_stock_in s LEFT JOIN sys_user u ON u.id = s.create_by LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE s.deleted_at IS NULL GROUP BY 1
UNION ALL
SELECT COALESCE(d.dept_name, '(无部门)'), 'payable', count(*)
  FROM biz_payable p LEFT JOIN sys_user u ON u.id = p.create_by LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE p.deleted_at IS NULL GROUP BY 1
UNION ALL
SELECT COALESCE(d.dept_name, '(无部门)'), 'voucher', count(*)
  FROM biz_voucher v LEFT JOIN sys_user u ON u.id = v.create_by LEFT JOIN sys_dept d ON d.id = u.dept_id
 WHERE v.deleted_at IS NULL GROUP BY 1
 ORDER BY 2,3 DESC;

\echo ''
\echo '-- C7) 若"导出按部门过滤"，3 档账号（laoliu_test 所在 TECH 子树）能导出多少？（对比 C1/C3/C4 的全量）'
WITH RECURSIVE sub AS (
    SELECT id FROM sys_dept WHERE dept_name = 'TECH' AND deleted_at IS NULL
    UNION ALL
    SELECT d.id FROM sys_dept d JOIN sub ON d.parent_id = sub.id WHERE d.deleted_at IS NULL
)
SELECT (SELECT count(*) FROM biz_stock_in s JOIN sys_user u ON u.id = s.create_by
         WHERE s.deleted_at IS NULL AND u.dept_id IN (SELECT id FROM sub)) AS stock_in_dept,
       (SELECT count(*) FROM biz_payable p JOIN sys_user u ON u.id = p.create_by
         WHERE p.deleted_at IS NULL AND u.dept_id IN (SELECT id FROM sub)) AS payable_dept,
       (SELECT count(*) FROM biz_voucher v JOIN sys_user u ON u.id = v.create_by
         WHERE v.deleted_at IS NULL AND u.dept_id IN (SELECT id FROM sub)) AS voucher_dept;

\echo ''
\echo '################ D) 导入侧：6 张主数据表有没有部门列（有才谈得上"只能导入本部门"）################'
SELECT table_name, count(*) FILTER (WHERE column_name = 'dept_id') AS dept_id_cols
  FROM information_schema.columns
 WHERE table_schema = 'public'
   AND table_name IN ('biz_supplier','biz_material','biz_unit','biz_account','biz_warehouse','biz_location','sys_dict_item')
 GROUP BY 1 ORDER BY 1;
