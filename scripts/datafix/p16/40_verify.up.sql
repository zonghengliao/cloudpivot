-- ============================================================================
-- P16 · ⑤ 收尾验收门（任何一项不为 0 → 抛异常，psql 退出码非 0）
-- ----------------------------------------------------------------------------
-- 口径与 scripts/verify/e2e03_reconcile.sql ①②⑤ 完全一致（⑤ 按 (物料,仓库) 聚合，
-- 与 erp_invariants.py C5 / inventory_audit.rs AUDIT_CTE 同口径）。
--
-- 卡面验收：
--   ① 库存 vs 流水差异 = 0
--   ② 逐桶对账差异   = 0
--   ④ 一物一码违例   = 0
-- 另加三道防回归：流水自洽、已过账单据必须有流水、负库存。
-- ============================================================================
\set ON_ERROR_STOP on
\pset footer off

\echo ''
\echo '===== 验收 ① 库存 vs 流水（按 物料×仓库）====='
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
\echo '===== 验收 ② 逐桶对账（成本账 vs 库位账，按桶键 FULL JOIN）====='
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
\echo '===== 验收 ③ 一物一码（按 物料×仓库 聚合）====='
SELECT count(*) AS diff_rows
  FROM (SELECT i.material_id, i.warehouse_id, SUM(i.qty) book_qty
          FROM biz_inventory i
          JOIN biz_material m ON m.id = i.material_id AND m.serial_required IS TRUE
         GROUP BY 1,2) book
  LEFT JOIN (SELECT s.material_id, s.warehouse_id, COUNT(*)::numeric AS actual_qty
               FROM biz_material_serial s
              WHERE s.status = 'IN_STOCK' AND s.deleted_at IS NULL
              GROUP BY 1,2) ser
    ON ser.material_id = book.material_id AND ser.warehouse_id = book.warehouse_id
 WHERE book.book_qty <> COALESCE(ser.actual_qty,0);

\echo ''
\echo '===== 验收 ④ 流水自洽 / ⑤ 孤儿库位账 / ⑥ 过账单据无流水 / ⑦ 负库存 ====='
SELECT '流水自洽(after-before=±qty)' AS 项, count(*) AS 违例
  FROM biz_inventory_txn
 WHERE (txn_type='IN'  AND after_qty - before_qty <> qty)
    OR (txn_type='OUT' AND before_qty - after_qty <> qty)
UNION ALL
SELECT '孤儿库位账（物料或仓库已不存在）', count(*) FROM biz_inventory_location l
 WHERE NOT EXISTS (SELECT 1 FROM biz_material  m WHERE m.id = l.material_id)
    OR NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = l.warehouse_id)
UNION ALL
SELECT '已过账单据无流水', count(*) FROM (
    SELECT d.id FROM biz_stock_in d WHERE d.status='POSTED' AND d.deleted_at IS NULL
      AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
    UNION ALL
    SELECT d.id FROM biz_stock_out d WHERE d.status='POSTED' AND d.deleted_at IS NULL
      AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
    UNION ALL
    SELECT d.id FROM biz_stock_take d WHERE d.status='POSTED' AND d.deleted_at IS NULL
      AND EXISTS (SELECT 1 FROM biz_stock_take_item i WHERE i.take_id = d.id AND i.diff_qty <> 0)
      AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
    UNION ALL
    SELECT d.id FROM biz_stock_transfer d WHERE d.status='DONE' AND d.deleted_at IS NULL
      AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
) x
UNION ALL
SELECT '负库存', count(*) FROM biz_inventory WHERE qty < 0;

\echo ''
\echo '===== 收口判定 ====='
DO $$
DECLARE
    n_qty int; n_amt numeric; n_bucket int; n_serial int;
    n_self int; n_orphan int; n_doc int; n_neg int;
BEGIN
    SELECT count(*), COALESCE(SUM(COALESCE(inv.qty,0) - COALESCE(txn.qty,0)),0)
      INTO n_qty, n_amt
      FROM (SELECT material_id, warehouse_id, SUM(qty) qty, SUM(amount) amt
              FROM biz_inventory GROUP BY 1,2) inv
      FULL JOIN (SELECT material_id, warehouse_id,
                        SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty,
                        SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END) amt
                   FROM biz_inventory_txn GROUP BY 1,2) txn
        ON txn.material_id = inv.material_id AND txn.warehouse_id = inv.warehouse_id
     WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0)
        OR COALESCE(inv.amt,0) <> COALESCE(txn.amt,0);

    SELECT count(*) INTO n_bucket
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

    SELECT count(*) INTO n_serial
      FROM (SELECT i.material_id, i.warehouse_id, SUM(i.qty) book_qty
              FROM biz_inventory i
              JOIN biz_material m ON m.id = i.material_id AND m.serial_required IS TRUE
             GROUP BY 1,2) book
      LEFT JOIN (SELECT s.material_id, s.warehouse_id, COUNT(*)::numeric AS actual_qty
                   FROM biz_material_serial s
                  WHERE s.status = 'IN_STOCK' AND s.deleted_at IS NULL
                  GROUP BY 1,2) ser
        ON ser.material_id = book.material_id AND ser.warehouse_id = book.warehouse_id
     WHERE book.book_qty <> COALESCE(ser.actual_qty,0);

    SELECT count(*) INTO n_self FROM biz_inventory_txn
     WHERE (txn_type='IN'  AND after_qty - before_qty <> qty)
        OR (txn_type='OUT' AND before_qty - after_qty <> qty);

    SELECT count(*) INTO n_orphan FROM biz_inventory_location l
     WHERE NOT EXISTS (SELECT 1 FROM biz_material  m WHERE m.id = l.material_id)
        OR NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = l.warehouse_id);

    SELECT count(*) INTO n_doc FROM (
        SELECT d.id FROM biz_stock_in d WHERE d.status='POSTED' AND d.deleted_at IS NULL
          AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
        UNION ALL
        SELECT d.id FROM biz_stock_out d WHERE d.status='POSTED' AND d.deleted_at IS NULL
          AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
        UNION ALL
        SELECT d.id FROM biz_stock_take d WHERE d.status='POSTED' AND d.deleted_at IS NULL
          AND EXISTS (SELECT 1 FROM biz_stock_take_item i WHERE i.take_id = d.id AND i.diff_qty <> 0)
          AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
        UNION ALL
        SELECT d.id FROM biz_stock_transfer d WHERE d.status='DONE' AND d.deleted_at IS NULL
          AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = d.id)
    ) x;

    SELECT count(*) INTO n_neg FROM biz_inventory WHERE qty < 0;

    IF n_qty > 0 OR n_amt <> 0 OR n_bucket > 0 OR n_serial > 0
       OR n_self > 0 OR n_orphan > 0 OR n_doc > 0 OR n_neg > 0 THEN
        RAISE EXCEPTION 'E2E-FIX-7 验收未归零：库存vs流水 % 行(数量差 %)/逐桶 % 行/一物一码 % 行/流水自洽 % 行/孤儿库位账 % 行/过账无流水 % 单/负库存 % 行',
            n_qty, n_amt, n_bucket, n_serial, n_self, n_orphan, n_doc, n_neg;
    END IF;
    RAISE NOTICE 'E2E-FIX-7 验收通过：库存vs流水 0 行 / 逐桶 0 行 / 一物一码 0 行 / 流水自洽 0 行 / 孤儿 0 行 / 过账无流水 0 单 / 负库存 0 行';
END $$;
