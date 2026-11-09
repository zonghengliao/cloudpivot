\pset pager off
\echo '===== cloudpivot_dev_clean 现况 ====='
SELECT (SELECT count(*) FROM schema_migrations) AS mig_rows,
       (SELECT count(*) FROM schema_migrations WHERE kind='versioned') AS ver,
       (SELECT max(version::int) FROM schema_migrations WHERE kind='versioned') AS maxver,
       (SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL) AS menus,
       (SELECT count(*) FROM sys_user WHERE deleted_at IS NULL) AS users,
       (SELECT count(*) FROM biz_voucher) AS vouchers,
       (SELECT count(*) FROM biz_receivable) AS ar,
       (SELECT count(*) FROM biz_inventory) AS inv,
       (SELECT count(*) FROM biz_stock_out) AS so;

\echo '===== 谁会连它（活动连接）====='
SELECT datname, usename, state, count(*), max(backend_start) FROM pg_stat_activity
WHERE datname IN ('cloudpivot_dev','cloudpivot_dev_clean') GROUP BY 1,2,3 ORDER BY 1;

\echo '===== 迁移版本缺口（dev 缺哪些）====='
SELECT 'dev' AS db, count(*) AS applied FROM dblink('dbname=cloudpivot_dev','select 1 from schema_migrations') AS t(x int);
