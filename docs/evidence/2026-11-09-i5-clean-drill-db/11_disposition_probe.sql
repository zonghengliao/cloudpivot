\pset pager off
\echo '===== 0. 库与迁移指纹 ====='
SELECT current_database() AS db,
       (SELECT count(*) FROM schema_migrations) AS mig_rows,
       (SELECT max(version::int) FROM schema_migrations WHERE kind='versioned') AS maxver;

\echo '===== 1. 卡片 SQL 原样（会被 MANUAL 占位 source_id 带出 21 行）====='
SELECT count(*) AS rows_from_card_sql
FROM biz_receivable r
WHERE r.deleted_at IS NULL
  AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL);

\echo '===== 2. 按 source_type 拆开：谁是真孤儿 ====='
SELECT r.source_type, count(*) AS rows, sum(r.amount) AS amount_sum
FROM biz_receivable r
WHERE r.deleted_at IS NULL AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL)
GROUP BY 1 ORDER BY 1;

\echo '===== 3. 真孤儿明细（SALES_OUT 且出库单已physical不存在/软删）====='
SELECT r.id, r.ar_no, r.source_type, r.source_no, r.source_id, c.name AS customer_name,
       r.amount, r.received_amount, r.unsettled_amount, r.status, r.period_code, r.created_at,
       (SELECT count(*) FROM biz_voucher v WHERE v.deleted_at IS NULL AND v.source_id = r.id) AS vouchers,
       (SELECT count(*) FROM biz_receipt_allocation a WHERE a.receivable_id = r.id) AS allocations
FROM biz_receivable r
LEFT JOIN biz_customer c ON c.id = r.customer_id
WHERE r.deleted_at IS NULL AND r.source_type = 'SALES_OUT' AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL)
ORDER BY r.ar_no;

\echo '===== 4. MANUAL 占位 source_id 证据（不是孤儿）====='
SELECT count(*) AS manual_live, count(DISTINCT source_id) AS distinct_source_id,
       count(DISTINCT source_no) AS distinct_source_no
FROM biz_receivable WHERE deleted_at IS NULL AND source_type = 'MANUAL';
SELECT r.ar_no, r.source_type, r.source_no, r.source_id
FROM biz_receivable r
WHERE r.deleted_at IS NULL AND r.source_type = 'MANUAL'
ORDER BY r.ar_no LIMIT 3;

\echo '===== 5. 库存 vs 流水：卡片 LEFT JOIN 口径（假差异，流水被库存桶行重复计入）====='
SELECT m.material_code, w.warehouse_code,
       SUM(i.qty) AS inv_sum,
       SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END) AS txn_sum_repeated,
       SUM(i.qty) - SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END) AS fake_diff
FROM biz_inventory i
LEFT JOIN biz_inventory_txn t ON t.material_id=i.material_id AND t.warehouse_id=i.warehouse_id
JOIN biz_material m ON m.id=i.material_id
JOIN biz_warehouse w ON w.id=i.warehouse_id
GROUP BY 1,2
HAVING SUM(i.qty) <> COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END), 0)
ORDER BY 1,2;

\echo '===== 6. 同一批物料×仓库：正确口径（两侧先聚合再 FULL JOIN）====='
WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2),
     txn AS (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty
               FROM biz_inventory_txn GROUP BY 1,2)
SELECT m.material_code, w.warehouse_code,
       COALESCE(inv.qty,0) AS inv_qty, COALESCE(txn.qty,0) AS txn_qty,
       COALESCE(inv.qty,0)-COALESCE(txn.qty,0) AS diff
FROM inv FULL JOIN txn USING (material_id, warehouse_id)
JOIN biz_material m ON m.id=COALESCE(inv.material_id,txn.material_id)
JOIN biz_warehouse w ON w.id=COALESCE(inv.warehouse_id,txn.warehouse_id)
WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0)
ORDER BY abs(COALESCE(inv.qty,0)-COALESCE(txn.qty,0)) DESC;

\echo '===== 7. 逐桶对账（成本账 biz_inventory vs 库位账 biz_inventory_location）====='
WITH cost AS (SELECT i.material_id, i.warehouse_id,
                     COALESCE(il.location_id::text,'(未分配)') bucket, SUM(i.qty) qty
                FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id
               GROUP BY 1,2,3),
     loc  AS (SELECT material_id, warehouse_id,
                     COALESCE(location_id::text,'(未分配)') bucket, SUM(qty) qty
                FROM biz_inventory_location GROUP BY 1,2,3)
SELECT m.material_code, w.warehouse_code, COALESCE(cost.bucket,loc.bucket) bucket,
       COALESCE(cost.qty,0) cost_qty, COALESCE(loc.qty,0) loc_qty,
       COALESCE(cost.qty,0)-COALESCE(loc.qty,0) AS diff
FROM cost FULL JOIN loc ON cost.material_id=loc.material_id
  AND cost.warehouse_id=loc.warehouse_id AND cost.bucket=loc.bucket
JOIN biz_material m ON m.id=COALESCE(cost.material_id,loc.material_id)
JOIN biz_warehouse w ON w.id=COALESCE(cost.warehouse_id,loc.warehouse_id)
WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0);

\echo '===== 8. 夹具前缀普查（历史演练夹具，方案 A 下整库归档保留）====='
SELECT 'biz_stock_in' t, substring(in_no from '^[A-Za-z0-9-]+-') prefix, count(*) FROM biz_stock_in GROUP BY 2
UNION ALL SELECT 'biz_stock_out', substring(out_no from '^[A-Za-z0-9-]+-'), count(*) FROM biz_stock_out GROUP BY 2
UNION ALL SELECT 'biz_receivable', substring(ar_no from '^[A-Za-z0-9-]+-'), count(*) FROM biz_receivable GROUP BY 2
ORDER BY 1,2;
