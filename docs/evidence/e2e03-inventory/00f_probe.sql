\echo '--- 已有库存期初按期间 ---'
SELECT period_code, count(*) AS rows, sum(qty) AS qty, sum(amount) AS amount
  FROM biz_opening_inventory GROUP BY 1 ORDER BY 1;
\echo '--- 会计期间 ---'
SELECT period_code, status, is_closed FROM biz_period ORDER BY period_code DESC LIMIT 12;
\echo '--- 科目 ---'
SELECT account_code, account_name, account_category, status FROM biz_account
 WHERE account_code IN ('1405','3103','3101','6602','6401') ORDER BY account_code;
\echo '--- 仓库 ---'
SELECT warehouse_code, warehouse_name, level, is_leaf, status FROM biz_warehouse WHERE deleted_at IS NULL ORDER BY created_at DESC LIMIT 15;
\echo '--- 序列号枚举/物料序列号列 ---'
SELECT column_name FROM information_schema.columns WHERE table_name='biz_material' AND column_name LIKE '%serial%';
