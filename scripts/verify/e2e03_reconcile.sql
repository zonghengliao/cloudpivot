-- E2E-03 库存域全量对账（145 cloudpivot_dev）
-- 口径唯一真源：docs/ERP-库存对账口径.md（FIX-7）
--   · biz_inventory 是「桶」表（唯一键 material+warehouse+COALESCE(location_id)）
--   · biz_inventory_txn 只有 (material, warehouse)，方向列 txn_type(IN/OUT)，qty 恒正
--   ⇒ 「库存 = 流水」只能按 (物料, 仓库) 聚合；逐桶只能比「成本账 vs 库位账」且必须按桶键 FULL JOIN
\set ON_ERROR_STOP on

\echo '===== ①-1 全账总量：库存表 vs 流水累计 ====='
SELECT (SELECT COALESCE(SUM(qty),0) FROM biz_inventory) AS inv_qty,
       (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0) FROM biz_inventory_txn) AS txn_qty,
       (SELECT COALESCE(SUM(qty),0) FROM biz_inventory)
         - (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0) FROM biz_inventory_txn) AS diff;

\echo '===== ①-2 全库差异行（历史遗留，E2E-03 前后行数必须不变）====='
WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2),
     txn AS (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty
               FROM biz_inventory_txn GROUP BY 1,2)
SELECT count(*) AS diff_rows, COALESCE(SUM(COALESCE(inv.qty,0)-COALESCE(txn.qty,0)),0) AS diff_qty
  FROM inv FULL JOIN txn USING (material_id, warehouse_id)
 WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0);

\echo '===== ①-3 本卡物料逐 (物料, 仓库)（期望 0 行差异）====='
WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2),
     txn AS (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty
               FROM biz_inventory_txn GROUP BY 1,2)
SELECT m.material_code, w.warehouse_code,
       COALESCE(inv.qty,0) AS inv_qty, COALESCE(txn.qty,0) AS txn_qty,
       COALESCE(inv.qty,0) - COALESCE(txn.qty,0) AS diff
  FROM inv FULL JOIN txn USING (material_id, warehouse_id)
  JOIN biz_material m ON m.id = COALESCE(inv.material_id, txn.material_id)
  JOIN biz_warehouse w ON w.id = COALESCE(inv.warehouse_id, txn.warehouse_id)
 WHERE m.material_code LIKE 'E2E03-%'
 ORDER BY 1,2;

\echo '===== ①-4 本卡物料差异行（期望 0 行）====='
WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2),
     txn AS (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty
               FROM biz_inventory_txn GROUP BY 1,2)
SELECT m.material_code, w.warehouse_code, COALESCE(inv.qty,0)-COALESCE(txn.qty,0) AS diff
  FROM inv FULL JOIN txn USING (material_id, warehouse_id)
  JOIN biz_material m ON m.id = COALESCE(inv.material_id, txn.material_id)
  JOIN biz_warehouse w ON w.id = COALESCE(inv.warehouse_id, txn.warehouse_id)
 WHERE m.material_code LIKE 'E2E03-%'
   AND COALESCE(inv.qty,0) <> COALESCE(txn.qty,0);

\echo '===== ②-1 逐桶成本账 vs 库位账：本卡物料差异行（期望 0 行）====='
WITH cost AS (SELECT i.material_id, i.warehouse_id,
                     COALESCE(il.location_id::text,'(未分配)') AS bucket, SUM(i.qty) qty
                FROM biz_inventory i
                LEFT JOIN biz_inventory_location il ON il.id = i.location_id
               GROUP BY 1,2,3),
     loc AS (SELECT material_id, warehouse_id,
                    COALESCE(location_id::text,'(未分配)') AS bucket, SUM(qty) qty
               FROM biz_inventory_location GROUP BY 1,2,3)
SELECT m.material_code, w.warehouse_code, COALESCE(cost.bucket, loc.bucket) AS bucket,
       COALESCE(cost.qty,0) AS cost_qty, COALESCE(loc.qty,0) AS loc_qty,
       COALESCE(cost.qty,0) - COALESCE(loc.qty,0) AS diff
  FROM cost FULL JOIN loc ON cost.material_id = loc.material_id
   AND cost.warehouse_id = loc.warehouse_id AND cost.bucket = loc.bucket
  JOIN biz_material m ON m.id = COALESCE(cost.material_id, loc.material_id)
  JOIN biz_warehouse w ON w.id = COALESCE(cost.warehouse_id, loc.warehouse_id)
 WHERE m.material_code LIKE 'E2E03-%' AND COALESCE(cost.qty,0) <> COALESCE(loc.qty,0);

\echo '===== ②-2 全库逐桶差异行数（历史遗留，前后必须不变）====='
WITH cost AS (SELECT i.material_id, i.warehouse_id,
                     COALESCE(il.location_id::text,'(未分配)') AS bucket, SUM(i.qty) qty
                FROM biz_inventory i
                LEFT JOIN biz_inventory_location il ON il.id = i.location_id
               GROUP BY 1,2,3),
     loc AS (SELECT material_id, warehouse_id,
                    COALESCE(location_id::text,'(未分配)') AS bucket, SUM(qty) qty
               FROM biz_inventory_location GROUP BY 1,2,3)
SELECT count(*) AS diff_rows
  FROM cost FULL JOIN loc ON cost.material_id = loc.material_id
   AND cost.warehouse_id = loc.warehouse_id AND cost.bucket = loc.bucket
 WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0);

\echo '===== ②-3 本卡物料的桶明细（人工核对）====='
SELECT m.material_code, w.warehouse_code,
       COALESCE(l.location_code,'(未分配)') AS bucket,
       i.qty AS cost_qty, i.avg_cost, i.amount,
       il.qty AS loc_ledger_qty
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id
  JOIN biz_warehouse w ON w.id=i.warehouse_id
  LEFT JOIN biz_inventory_location il ON il.id=i.location_id
  LEFT JOIN biz_location l ON l.id=il.location_id
 WHERE m.material_code LIKE 'E2E03-%'
 ORDER BY 1,2,3;

\echo '===== ③ 金额：库存表 amount vs 流水入-出累计（本卡物料，期望 0 差异）====='
WITH inv AS (SELECT material_id, warehouse_id, SUM(amount) amt FROM biz_inventory GROUP BY 1,2),
     txn AS (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END) amt
               FROM biz_inventory_txn GROUP BY 1,2)
SELECT m.material_code, COALESCE(inv.amt,0) AS inv_amount, COALESCE(txn.amt,0) AS txn_amount,
       COALESCE(inv.amt,0)-COALESCE(txn.amt,0) AS diff
  FROM inv FULL JOIN txn USING (material_id, warehouse_id)
  JOIN biz_material m ON m.id = COALESCE(inv.material_id, txn.material_id)
 WHERE m.material_code LIKE 'E2E03-%'
   AND COALESCE(inv.amt,0) <> COALESCE(txn.amt,0);

\echo '===== ④ 流水自洽：after-before = ±qty（全库，期望 0 行）====='
SELECT count(*) AS bad_rows FROM biz_inventory_txn
 WHERE (txn_type='IN'  AND after_qty - before_qty <> qty)
    OR (txn_type='OUT' AND before_qty - after_qty <> qty);

\echo '===== ⑤ 一物一码不变式：qty == count(IN_STOCK)（全库，期望 0 行）====='
SELECT m.material_code, w.warehouse_code, i.qty, s.cnt
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id AND m.serial_required
  JOIN biz_warehouse w ON w.id=i.warehouse_id
  JOIN LATERAL (SELECT count(*)::numeric(20,4) AS cnt FROM biz_material_serial s2
                 WHERE s2.material_id=i.material_id AND s2.warehouse_id=i.warehouse_id
                   AND s2.status='IN_STOCK' AND s2.deleted_at IS NULL) s ON true
 WHERE i.qty <> s.cnt;

\echo '===== ⑥ FIX-4：期初桶必须真实存在于成本账（0 行 = 合规）====='
SELECT oi.period_code, m.material_code, oi.qty, oi.location_row_id
  FROM biz_opening_inventory oi
  JOIN biz_material m ON m.id=oi.material_id
 WHERE oi.location_row_id IS NOT NULL
   AND NOT EXISTS (SELECT 1 FROM biz_inventory i WHERE i.location_id = oi.location_row_id AND i.material_id = oi.material_id);

\echo '===== ⑦ FIX-7：销售出库流水必须记 SALE_OUT（out_type=OTHER + source_type=SALES）====='
SELECT count(*) AS wrong_biz_type
  FROM biz_stock_out o JOIN biz_inventory_txn t ON t.doc_id = o.id
 WHERE o.source_type = 'SALES' AND o.out_type = 'OTHER' AND t.biz_type <> 'SALE_OUT';

\echo '===== ⑧ 单据状态 vs 流水：已过账的库存单必须有流水（0 行 = 合规）====='
SELECT 'STOCK_IN' AS doc, count(*) AS missing FROM biz_stock_in d
 WHERE d.status='POSTED' AND d.deleted_at IS NULL AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id=d.id)
UNION ALL
SELECT 'STOCK_OUT', count(*) FROM biz_stock_out d
 WHERE d.status='POSTED' AND d.deleted_at IS NULL AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id=d.id)
UNION ALL
SELECT 'STOCK_TAKE', count(*) FROM biz_stock_take d
 WHERE d.status='POSTED' AND d.deleted_at IS NULL
   AND EXISTS (SELECT 1 FROM biz_stock_take_item i WHERE i.take_id=d.id AND i.diff_qty <> 0)
   AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id=d.id)
UNION ALL
SELECT 'STOCK_TRANSFER', count(*) FROM biz_stock_transfer d
 WHERE d.status='DONE' AND d.deleted_at IS NULL AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id=d.id);

\echo '===== ⑨ 凭证借贷平衡（全库不平的凭证，期望 0 行）====='
SELECT v.voucher_no, SUM(e.debit_amount) AS debit, SUM(e.credit_amount) AS credit
  FROM biz_voucher v JOIN biz_voucher_entry e ON e.voucher_id=v.id
 WHERE v.status='POSTED' AND v.deleted_at IS NULL
 GROUP BY v.id, v.voucher_no
HAVING SUM(e.debit_amount) <> SUM(e.credit_amount);

\echo '===== ⑩ 负库存（期望 0 行）====='
SELECT m.material_code, w.warehouse_code, i.qty FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id JOIN biz_warehouse w ON w.id=i.warehouse_id
 WHERE i.qty < 0;

\echo '===== ⑪ 本卡演练流水全轨迹（审计）====='
SELECT m.material_code, w.warehouse_code, t.txn_type, t.biz_type, t.doc_no,
       t.qty, t.before_qty, t.after_qty, t.unit_cost, t.amount, t.period_code
  FROM biz_inventory_txn t
  JOIN biz_material m ON m.id=t.material_id
  JOIN biz_warehouse w ON w.id=t.warehouse_id
 WHERE m.material_code LIKE 'E2E03-%'
 ORDER BY m.material_code, t.created_at;
