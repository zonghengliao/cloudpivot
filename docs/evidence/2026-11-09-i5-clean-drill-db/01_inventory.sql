\pset pager off
\echo '===== A0. 库概况 ====='
SELECT current_database() AS db, (SELECT count(*) FROM biz_receivable WHERE deleted_at IS NULL) AS ar_live,
       (SELECT count(*) FROM biz_stock_out WHERE deleted_at IS NULL) AS so_live,
       (SELECT count(*) FROM biz_inventory) AS inv_rows,
       (SELECT count(*) FROM biz_inventory_txn) AS txn_rows;

\echo '===== A1. 孤儿应收（卡片口径：source_id 指向已删/不存在的出库单）====='
SELECT r.id, r.ar_no, r.source_type, r.source_no, r.source_id, r.amount, r.status, r.created_at,
       (SELECT count(*) FROM biz_stock_out s WHERE s.id = r.source_id) AS so_rows_any,
       (SELECT s.out_no FROM biz_stock_out s WHERE s.id = r.source_id) AS so_out_no,
       (SELECT s.deleted_at FROM biz_stock_out s WHERE s.id = r.source_id) AS so_deleted_at
FROM biz_receivable r
WHERE r.deleted_at IS NULL
  AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL)
ORDER BY r.ar_no;

\echo '===== A2. 孤儿应收（全部来源都对不上的，含非 SALES）====='
SELECT r.ar_no, r.source_type, r.source_no, r.source_id, r.amount
FROM biz_receivable r
WHERE r.deleted_at IS NULL AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL)
  AND NOT EXISTS (SELECT 1 FROM biz_sales_order o WHERE o.id = r.source_id)
ORDER BY 1;

\echo '===== B1. 库存 vs 流水（正确口径：两侧先聚合再 FULL JOIN，物料×仓库）====='
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

\echo '===== B2. 差异组数（正确口径）====='
WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2),
     txn AS (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty
               FROM biz_inventory_txn GROUP BY 1,2)
SELECT count(*) AS diff_groups
FROM inv FULL JOIN txn USING (material_id, warehouse_id)
WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0);

\echo '===== B3. 卡片给的 LEFT JOIN 写法（会把流水按库存行重复计入 → 假差异）====='
SELECT count(*) AS rows_from_card_sql
FROM (
  SELECT i.material_id, i.warehouse_id, SUM(i.qty) AS inv_qty,
         COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END),0) AS txn_qty
  FROM biz_inventory i
  LEFT JOIN biz_inventory_txn t ON t.material_id=i.material_id AND t.warehouse_id=i.warehouse_id
  GROUP BY 1,2
  HAVING SUM(i.qty) <> COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END),0)
) x;

\echo '===== B4. 逐桶：成本账 vs 库位账（物料×仓库×库位）====='
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
WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0)
ORDER BY abs(COALESCE(cost.qty,0)-COALESCE(loc.qty,0)) DESC;

\echo '===== C1. 夹具前缀普查（单据号维度）====='
SELECT 'biz_stock_out' AS t, substring(out_no from '^[A-Za-z0-9]+-') AS prefix, count(*) FROM biz_stock_out GROUP BY 2
UNION ALL
SELECT 'biz_stock_in', substring(in_no from '^[A-Za-z0-9]+-'), count(*) FROM biz_stock_in GROUP BY 2
UNION ALL
SELECT 'biz_receivable', substring(ar_no from '^[A-Za-z0-9]+-'), count(*) FROM biz_receivable GROUP BY 2
ORDER BY 1,3 DESC;
