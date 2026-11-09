\pset pager off
\echo '===== 1. 库概况（cloudpivot_dev）====='
SELECT current_database() AS db,
       (SELECT count(*) FROM biz_receivable WHERE deleted_at IS NULL) AS ar_live,
       (SELECT count(*) FROM biz_receivable WHERE deleted_at IS NOT NULL) AS ar_deleted,
       (SELECT count(*) FROM biz_stock_out) AS so_all,
       (SELECT count(*) FROM biz_stock_out WHERE deleted_at IS NOT NULL) AS so_deleted,
       (SELECT count(*) FROM biz_inventory) AS inv_rows,
       (SELECT count(*) FROM biz_inventory_txn) AS txn_rows,
       (SELECT count(*) FROM biz_voucher WHERE deleted_at IS NULL) AS vouchers;

\echo '===== 2. 孤儿应收（卡片口径，按 source_type 分类）====='
SELECT r.source_type, count(*) AS rows,
       sum(r.amount) AS amount_sum,
       min(r.created_at) AS first_at, max(r.created_at) AS last_at
FROM biz_receivable r
WHERE r.deleted_at IS NULL AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL)
GROUP BY 1 ORDER BY 1;

\echo '===== 2b. 真孤儿明细（SALES_OUT 来源 → 出库单已不存在）====='
SELECT r.id, r.ar_no, r.customer_id, c.name AS customer_name, r.source_no, r.source_id,
       r.amount, r.received_amount, r.unsettled_amount, r.status, r.period_code, r.created_at
FROM biz_receivable r
LEFT JOIN biz_customer c ON c.id = r.customer_id
WHERE r.deleted_at IS NULL AND r.source_type = 'SALES_OUT' AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL)
ORDER BY r.ar_no;

\echo '===== 2c. 这 5 张是否被凭证/收款引用（删前核对，决定能否软删）====='
SELECT r.ar_no,
       (SELECT count(*) FROM biz_voucher v
         WHERE v.deleted_at IS NULL AND v.source_id = r.id) AS vouchers,
       (SELECT count(*) FROM biz_receipt_allocation a WHERE a.receivable_id = r.id) AS allocations
FROM biz_receivable r
WHERE r.deleted_at IS NULL AND r.source_type = 'SALES_OUT' AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL)
ORDER BY r.ar_no;

\echo '===== 2d. MANUAL 应收的 source_id 是服务端生成的占位 UUID（不是孤儿）====='
SELECT count(*) AS manual_live, count(DISTINCT source_id) AS distinct_source_id
FROM biz_receivable WHERE deleted_at IS NULL AND source_type = 'MANUAL';

\echo '===== 3A. 卡片给的 LEFT JOIN 写法（会把流水按库存桶行重复计入）====='
SELECT count(*) AS rows_from_card_sql FROM (
  SELECT i.material_id, i.warehouse_id,
         SUM(i.qty) AS inv_sum,
         COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END), 0) AS txn_sum
  FROM biz_inventory i
  LEFT JOIN biz_inventory_txn t ON t.material_id=i.material_id AND t.warehouse_id=i.warehouse_id
  GROUP BY 1,2
  HAVING SUM(i.qty) <> COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END), 0)
) x;

\echo '===== 3B. 正确口径（两侧先聚合再 FULL JOIN，物料×仓库）====='
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

\echo '===== 3C. 逐桶：成本账 vs 库位账（物料×仓库×库位）====='
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

\echo '===== 4. 夹具普查：单号前缀（历史演练夹具）====='
SELECT 'biz_stock_in' t, substring(in_no from '^[A-Za-z0-9-]+-') prefix, count(*) FROM biz_stock_in GROUP BY 2
UNION ALL SELECT 'biz_stock_out', substring(out_no from '^[A-Za-z0-9-]+-'), count(*) FROM biz_stock_out GROUP BY 2
UNION ALL SELECT 'biz_receivable', substring(ar_no from '^[A-Za-z0-9-]+-'), count(*) FROM biz_receivable GROUP BY 2
UNION ALL SELECT 'biz_voucher', substring(voucher_no from '[A-Za-z]*-'), count(*) FROM biz_voucher WHERE deleted_at IS NULL GROUP BY 2
ORDER BY 1,2;

\echo '===== 4b. 夹具普查：物料/仓库/客户前缀====='
SELECT 'material' t, split_part(material_code,'-',1) prefix, count(*) FROM biz_material GROUP BY 2
UNION ALL SELECT 'warehouse', split_part(warehouse_code,'-',1), count(*) FROM biz_warehouse GROUP BY 2
ORDER BY 1,2;

\echo '===== 5. 13 域演练主链路（要保留）：最近 24h 建的单据 ====='
SELECT 'inv_txn' t, count(*), min(created_at), max(created_at) FROM biz_inventory_txn WHERE created_at > now() - interval '24 hours'
UNION ALL SELECT 'stock_out', count(*), min(created_at), max(created_at) FROM biz_stock_out WHERE created_at > now() - interval '24 hours'
UNION ALL SELECT 'receivable', count(*), min(created_at), max(created_at) FROM biz_receivable WHERE created_at > now() - interval '24 hours'
UNION ALL SELECT 'voucher', count(*), min(created_at), max(created_at) FROM biz_voucher WHERE created_at > now() - interval '24 hours';

\echo '===== 6. 出库单现状（孤儿 AR 的 source 侧）====='
SELECT out_no, status, source_type, out_date, created_at FROM biz_stock_out ORDER BY created_at;
