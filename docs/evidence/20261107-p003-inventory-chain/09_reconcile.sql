-- P0-03 库存链全量对账（145 cloudpivot_dev）
-- 口径说明：biz_inventory 是按 (material_id, warehouse_id, 库位桶) 分行存储的
--          （location_id 为空 = 未分配桶），biz_inventory_txn 只有 (material, warehouse)。
--          因此「库存表 vs 流水」只能在 (material, warehouse) 粒度相加后比较。

\echo '===== ⑦-0 全账总量：库存表合计 vs 流水累计 ====='
SELECT
  (SELECT COALESCE(SUM(qty),0) FROM biz_inventory)                                    AS inv_qty_total,
  (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0)
     FROM biz_inventory_txn)                                                          AS txn_qty_total,
  (SELECT COALESCE(SUM(qty),0) FROM biz_inventory)
    - (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0)
         FROM biz_inventory_txn)                                                      AS diff;

\echo '===== ⑦-1 逐 (物料, 仓库) 对账（全量，含所有历史数据）====='
SELECT m.material_code, w.warehouse_code,
       SUM(i.qty)                                                                    AS inv_qty,
       (SELECT COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END),0)
          FROM biz_inventory_txn t
         WHERE t.material_id=i.material_id AND t.warehouse_id=i.warehouse_id)        AS txn_qty,
       SUM(i.qty) - (SELECT COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END),0)
                       FROM biz_inventory_txn t
                      WHERE t.material_id=i.material_id AND t.warehouse_id=i.warehouse_id) AS diff
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id
  JOIN biz_warehouse w ON w.id=i.warehouse_id
 GROUP BY m.material_code, w.warehouse_code, i.material_id, i.warehouse_id
 ORDER BY m.material_code, w.warehouse_code;

\echo '===== ⑦-2 差异行（期望 0 行）====='
SELECT m.material_code, w.warehouse_code,
       SUM(i.qty) AS inv_qty,
       (SELECT COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END),0)
          FROM biz_inventory_txn t
         WHERE t.material_id=i.material_id AND t.warehouse_id=i.warehouse_id) AS txn_qty,
       SUM(i.qty) - (SELECT COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END),0)
                       FROM biz_inventory_txn t
                      WHERE t.material_id=i.material_id AND t.warehouse_id=i.warehouse_id) AS diff
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id
  JOIN biz_warehouse w ON w.id=i.warehouse_id
 GROUP BY m.material_code, w.warehouse_code, i.material_id, i.warehouse_id
HAVING SUM(i.qty) <> (SELECT COALESCE(SUM(CASE WHEN t.txn_type='IN' THEN t.qty ELSE -t.qty END),0)
                        FROM biz_inventory_txn t
                       WHERE t.material_id=i.material_id AND t.warehouse_id=i.warehouse_id);

\echo '===== ⑦-3 金额口径：库存表金额合计 vs 流水入-出金额累计（全量）====='
SELECT (SELECT COALESCE(SUM(amount),0) FROM biz_inventory) AS inv_amount_total,
       (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END),0)
          FROM biz_inventory_txn)                          AS txn_amount_total,
       (SELECT COALESCE(SUM(amount),0) FROM biz_inventory)
         - (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END),0)
              FROM biz_inventory_txn)                      AS diff;

\echo '===== ⑦-4 流水自洽：after_qty - before_qty 是否等于 ±qty（期望 0 行异常）====='
SELECT id, doc_no, txn_type, qty, before_qty, after_qty
  FROM biz_inventory_txn
 WHERE (txn_type='IN'  AND after_qty - before_qty <> qty)
    OR (txn_type='OUT' AND before_qty - after_qty <> qty);

\echo '===== ⑦-5 演练物料逐笔流水（审计轨迹）====='
SELECT w.warehouse_code, t.txn_type, t.biz_type, t.doc_no, t.qty,
       t.before_qty, t.after_qty, t.unit_cost, t.amount, t.period_code
  FROM biz_inventory_txn t
  JOIN biz_material m ON m.id=t.material_id
  JOIN biz_warehouse w ON w.id=t.warehouse_id
 WHERE m.material_code='DEMO-INV01'
 ORDER BY t.created_at, t.doc_no;

\echo '===== ⑧-1 库位账 vs 库存账（逐物料+仓库，期望 0 行差异）====='
SELECT m.material_code, w.warehouse_code,
       SUM(i.qty) AS inv_qty,
       (SELECT COALESCE(SUM(il.qty),0) FROM biz_inventory_location il
         WHERE il.material_id=i.material_id AND il.warehouse_id=i.warehouse_id) AS loc_qty,
       SUM(i.qty) - (SELECT COALESCE(SUM(il.qty),0) FROM biz_inventory_location il
                      WHERE il.material_id=i.material_id AND il.warehouse_id=i.warehouse_id) AS diff
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id
  JOIN biz_warehouse w ON w.id=i.warehouse_id
 GROUP BY m.material_code, w.warehouse_code, i.material_id, i.warehouse_id
HAVING SUM(i.qty) <> (SELECT COALESCE(SUM(il.qty),0) FROM biz_inventory_location il
                       WHERE il.material_id=i.material_id AND il.warehouse_id=i.warehouse_id);

\echo '===== ⑧-2 逐库位桶对账（biz_inventory.location_id → biz_inventory_location.id，期望 0 行差异）====='
SELECT m.material_code, w.warehouse_code,
       COALESCE(l.location_code,'(未分配)') AS location, i.qty AS inv_qty, il.qty AS loc_qty,
       i.qty - il.qty AS diff
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id
  JOIN biz_warehouse w ON w.id=i.warehouse_id
  LEFT JOIN biz_inventory_location il ON il.id=i.location_id
  LEFT JOIN biz_location l ON l.id=il.location_id
 WHERE i.qty <> COALESCE(il.qty,0)
 ORDER BY m.material_code, w.warehouse_code, location;

\echo '===== ⑧-3 演练物料：库存账 / 库位账 明细（人工核对用）====='
SELECT w.warehouse_code, COALESCE(l.location_code,'(未分配)') AS loc,
       i.qty AS inv_qty, i.avg_cost, i.amount,
       il.qty AS loc_ledger_qty
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id
  JOIN biz_warehouse w ON w.id=i.warehouse_id
  LEFT JOIN biz_inventory_location il ON il.id=i.location_id
  LEFT JOIN biz_location l ON l.id=il.location_id
 WHERE m.material_code='DEMO-INV01'
 ORDER BY w.warehouse_code, loc;

\echo '===== 附加：库存为负（期望 0 行）====='
SELECT m.material_code, w.warehouse_code, i.qty
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id
  JOIN biz_warehouse w ON w.id=i.warehouse_id
 WHERE i.qty < 0;
