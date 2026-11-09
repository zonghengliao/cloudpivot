-- ============================================================================
-- P16 · ② 只读盘点报告（跑完不改一行）
-- ----------------------------------------------------------------------------
-- 口径唯一真源：docs/ERP-库存对账口径.md + scripts/verify/e2e03_reconcile.sql ① ② ⑤
--   · biz_inventory 是「桶」表：唯一键 (material, warehouse, COALESCE(location_id))
--   · biz_inventory_txn **没有库位列**，方向列 txn_type(IN/OUT)、qty 恒正
--   ⇒ 「库存 = 流水」只能按 (物料, 仓库) 聚合；逐桶只比「成本账 vs 库位账」
--   · 一物一码（⑤）**按 (物料, 仓库) 聚合**比 —— 与 erp_invariants.py C5、
--     inventory_audit.rs 的 AUDIT_CTE 同口径；逐桶比会在多桶物料上假红
-- ============================================================================
\set ON_ERROR_STOP on
\pset footer off

\echo ''
\echo '===== ①-1 库存 vs 流水：逐 (物料, 仓库) 差异 ====='
SELECT m.material_code, w.warehouse_code,
       COALESCE(inv.qty,0)  AS inv_qty,
       COALESCE(txn.qty,0)  AS txn_qty,
       COALESCE(inv.qty,0) - COALESCE(txn.qty,0) AS qty_diff,
       COALESCE(inv.amt,0) - COALESCE(txn.amt,0) AS amt_diff
  FROM (SELECT material_id, warehouse_id, SUM(qty) qty, SUM(amount) amt
          FROM biz_inventory GROUP BY 1,2) inv
  FULL JOIN (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty,
                    SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END) amt
               FROM biz_inventory_txn GROUP BY 1,2) txn
    ON txn.material_id = inv.material_id AND txn.warehouse_id = inv.warehouse_id
  LEFT JOIN biz_material  m ON m.id = COALESCE(inv.material_id, txn.material_id)
  LEFT JOIN biz_warehouse w ON w.id = COALESCE(inv.warehouse_id, txn.warehouse_id)
 WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0)
    OR COALESCE(inv.amt,0) <> COALESCE(txn.amt,0)
 ORDER BY 1,2;

\echo ''
\echo '===== ①-2 差异行数合计（清账后必须 = 0 行 / 0 qty / 0 amt）====='
SELECT count(*) AS diff_rows,
       COALESCE(SUM(COALESCE(inv.qty,0) - COALESCE(txn.qty,0)),0) AS qty_diff,
       COALESCE(SUM(COALESCE(inv.amt,0) - COALESCE(txn.amt,0)),0) AS amt_diff
  FROM (SELECT material_id, warehouse_id, SUM(qty) qty, SUM(amount) amt
          FROM biz_inventory GROUP BY 1,2) inv
  FULL JOIN (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty,
                    SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END) amt
               FROM biz_inventory_txn GROUP BY 1,2) txn
    ON txn.material_id = inv.material_id AND txn.warehouse_id = inv.warehouse_id
 WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0)
    OR COALESCE(inv.amt,0) <> COALESCE(txn.amt,0);

\echo ''
\echo '===== ② 逐桶对账：成本账 vs 库位账（按桶键 FULL JOIN）====='
SELECT m.material_code, w.warehouse_code,
       CASE WHEN COALESCE(cost.bucket, loc.bucket) = '~NULL~'
            THEN '(未分配)' ELSE COALESCE(l.location_code,'?') END AS bucket,
       COALESCE(cost.qty,0) AS cost_qty,
       COALESCE(loc.qty,0)  AS loc_qty,
       COALESCE(cost.qty,0) - COALESCE(loc.qty,0) AS diff
  FROM (SELECT i.material_id, i.warehouse_id,
               COALESCE(il.location_id::text,'~NULL~') AS bucket, SUM(i.qty) qty
          FROM biz_inventory i
          LEFT JOIN biz_inventory_location il ON il.id = i.location_id
         GROUP BY 1,2,3) cost
  FULL JOIN (SELECT material_id, warehouse_id,
                    COALESCE(location_id::text,'~NULL~') AS bucket, SUM(qty) qty
               FROM biz_inventory_location GROUP BY 1,2,3) loc
    ON loc.material_id = cost.material_id
   AND loc.warehouse_id = cost.warehouse_id
   AND loc.bucket = cost.bucket
  LEFT JOIN biz_material  m ON m.id = COALESCE(cost.material_id, loc.material_id)
  LEFT JOIN biz_warehouse w ON w.id = COALESCE(cost.warehouse_id, loc.warehouse_id)
  LEFT JOIN (SELECT id::text AS b, location_code FROM biz_location) l
    ON l.b = COALESCE(cost.bucket, loc.bucket)
 WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0)
 ORDER BY 1,2,3;

\echo ''
\echo '===== ②-2 逐桶差异行数（清账后必须 = 0）====='
SELECT count(*) AS diff_rows
  FROM (SELECT i.material_id, i.warehouse_id,
               COALESCE(il.location_id::text,'~NULL~') AS bucket, SUM(i.qty) qty
          FROM biz_inventory i
          LEFT JOIN biz_inventory_location il ON il.id = i.location_id
         GROUP BY 1,2,3) cost
  FULL JOIN (SELECT material_id, warehouse_id,
                    COALESCE(location_id::text,'~NULL~') AS bucket, SUM(qty) qty
               FROM biz_inventory_location GROUP BY 1,2,3) loc
    ON loc.material_id = cost.material_id
   AND loc.warehouse_id = cost.warehouse_id
   AND loc.bucket = cost.bucket
 WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0);

\echo ''
\echo '===== ③ 一物一码：按 (物料, 仓库) 聚合（清账后必须 = 0 行）====='
SELECT m.material_code, w.warehouse_code, book.book_qty, COALESCE(ser.actual_qty,0) AS actual_qty,
       book.book_qty - COALESCE(ser.actual_qty,0) AS diff, book.bucket_count
  FROM (SELECT i.material_id, i.warehouse_id, SUM(i.qty) book_qty, COUNT(*) bucket_count
          FROM biz_inventory i
          JOIN biz_material m ON m.id = i.material_id AND m.serial_required IS TRUE
         GROUP BY 1,2) book
  JOIN biz_material  m ON m.id = book.material_id
  JOIN biz_warehouse w ON w.id = book.warehouse_id
  LEFT JOIN (SELECT s.material_id, s.warehouse_id, COUNT(*)::numeric AS actual_qty
               FROM biz_material_serial s
               WHERE s.status = 'IN_STOCK' AND s.deleted_at IS NULL
              GROUP BY 1,2) ser
    ON ser.material_id = book.material_id AND ser.warehouse_id = book.warehouse_id
 WHERE book.book_qty <> COALESCE(ser.actual_qty,0)
 ORDER BY 1,2;

\echo ''
\echo '===== ④ 流水自洽（after-before = ±qty，清账后必须 = 0 行）====='
SELECT count(*) AS bad_rows
  FROM biz_inventory_txn
 WHERE (txn_type='IN'  AND after_qty - before_qty <> qty)
    OR (txn_type='OUT' AND before_qty - after_qty <> qty);

\echo ''
\echo '===== ⑤ 孤儿行（主数据已被硬删，盘点/对账带噪的根源）====='
SELECT '库位账无物料' AS kind, count(*) FROM biz_inventory_location l
 WHERE NOT EXISTS (SELECT 1 FROM biz_material m WHERE m.id = l.material_id)
UNION ALL
SELECT '库位账无仓库', count(*) FROM biz_inventory_location l
 WHERE NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = l.warehouse_id)
UNION ALL
SELECT '成本账无物料', count(*) FROM biz_inventory i
 WHERE NOT EXISTS (SELECT 1 FROM biz_material m WHERE m.id = i.material_id)
UNION ALL
SELECT '流水无物料', count(*) FROM biz_inventory_txn x
 WHERE NOT EXISTS (SELECT 1 FROM biz_material m WHERE m.id = x.material_id)
UNION ALL
SELECT '流水单据已不存在', count(*) FROM biz_inventory_txn x
 WHERE (x.doc_type='STOCK_IN'     AND NOT EXISTS (SELECT 1 FROM biz_stock_in d     WHERE d.id = x.doc_id))
    OR (x.doc_type='STOCK_OUT'    AND NOT EXISTS (SELECT 1 FROM biz_stock_out d    WHERE d.id = x.doc_id))
    OR (x.doc_type='STOCK_TAKE'   AND NOT EXISTS (SELECT 1 FROM biz_stock_take d   WHERE d.id = x.doc_id))
    OR (x.doc_type='STOCK_TRANSFER' AND NOT EXISTS (SELECT 1 FROM biz_stock_transfer d WHERE d.id = x.doc_id));

\echo ''
\echo '===== ⑥ 过账单据无流水 / 负库存（清账后必须 0）====='
SELECT 'STOCK_IN 无流水' AS kind, count(*) FROM biz_stock_in d
 WHERE d.status='POSTED' AND d.deleted_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
UNION ALL
SELECT 'STOCK_OUT 无流水', count(*) FROM biz_stock_out d
 WHERE d.status='POSTED' AND d.deleted_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
UNION ALL
SELECT 'STOCK_TAKE 无流水', count(*) FROM biz_stock_take d
 WHERE d.status='POSTED' AND d.deleted_at IS NULL
   AND EXISTS (SELECT 1 FROM biz_stock_take_item i WHERE i.take_id = d.id AND i.diff_qty <> 0)
   AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
UNION ALL
SELECT 'STOCK_TRANSFER 无流水', count(*) FROM biz_stock_transfer d
 WHERE d.status='DONE' AND d.deleted_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
UNION ALL
SELECT '负库存', count(*) FROM biz_inventory WHERE qty < 0;
