\pset pager off
\echo '===== dev 现况快照 ====='
SELECT (SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL) AS menus,
       (SELECT count(*) FROM sys_user WHERE deleted_at IS NULL) AS users,
       (SELECT count(*) FROM biz_voucher WHERE deleted_at IS NULL) AS vouchers,
       (SELECT count(*) FROM biz_stock_out) AS so_all,
       (SELECT count(*) FROM biz_stock_out WHERE deleted_at IS NOT NULL) AS so_deleted,
       (SELECT count(*) FROM biz_receivable) AS ar_all,
       (SELECT count(*) FROM biz_receivable WHERE deleted_at IS NOT NULL) AS ar_deleted;
SELECT 'inventory' t, min(created_at) mn, max(created_at) mx FROM biz_inventory
UNION ALL SELECT 'inv_txn', min(created_at), max(created_at) FROM biz_inventory_txn
UNION ALL SELECT 'stock_out', min(created_at), max(created_at) FROM biz_stock_out
UNION ALL SELECT 'receivable', min(created_at), max(created_at) FROM biz_receivable
UNION ALL SELECT 'voucher', min(created_at), max(created_at) FROM biz_voucher;
SELECT kind, count(*), max(version) FROM schema_migrations GROUP BY kind;
\echo '===== 库存 21 行明细 ====='
SELECT m.material_code, w.warehouse_code, i.location_id, i.qty, i.amount
FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id JOIN biz_warehouse w ON w.id=i.warehouse_id
ORDER BY 1,2;
\echo '===== 流水 48 行按 biz_type 归纳 ====='
SELECT biz_type, txn_type, count(*), sum(qty) FROM biz_inventory_txn GROUP BY 1,2 ORDER BY 1,2;
