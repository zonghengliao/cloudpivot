-- P0-03 终局对账（145 cloudpivot_dev，修复 + 数据修复之后）
-- 口径：
--   * `biz_inventory` 按 (material, warehouse, 库位桶) 分行；`biz_inventory_txn` 只有 (material, warehouse)
--     ⇒ 「库存表 = 流水累计」只能在 (material, warehouse) 粒度相加后比较。
--   * 库位账 `biz_inventory_location` 每行有一个 `location_id`（库位字典 id，NULL = 未分配桶）；
--     成本账的桶由 `biz_inventory.location_id` → `biz_inventory_location.id` → 字典 id 换算，
--     ⇒ 逐桶对账要按「物料 + 仓库 + 库位字典」分组后 FULL JOIN。

\echo '===== ⑦-0 全账总量：库存表合计 vs 流水累计 ====='
SELECT (SELECT COALESCE(SUM(qty),0) FROM biz_inventory)                                  AS inv_qty,
       (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0)
          FROM biz_inventory_txn)                                                        AS txn_qty,
       (SELECT COALESCE(SUM(qty),0) FROM biz_inventory)
         - (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END),0)
              FROM biz_inventory_txn)                                                    AS diff;

\echo '===== ⑦-1 逐 (物料, 仓库)：库存表 qty vs 流水累计（全量历史）====='
WITH inv AS (
  SELECT i.material_id, i.warehouse_id, SUM(i.qty) AS inv_qty, SUM(i.amount) AS inv_amount
    FROM biz_inventory i GROUP BY 1,2),
txn AS (
  SELECT material_id, warehouse_id,
         SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END)       AS txn_qty,
         SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END) AS txn_amount
    FROM biz_inventory_txn GROUP BY 1,2)
SELECT m.material_code, w.warehouse_code,
       COALESCE(inv.inv_qty,0)   AS inv_qty,
       COALESCE(txn.txn_qty,0)   AS txn_qty,
       COALESCE(inv.inv_qty,0) - COALESCE(txn.txn_qty,0) AS diff
  FROM inv FULL JOIN txn ON inv.material_id=txn.material_id AND inv.warehouse_id=txn.warehouse_id
  JOIN biz_material m ON m.id=COALESCE(inv.material_id,txn.material_id)
  JOIN biz_warehouse w ON w.id=COALESCE(inv.warehouse_id,txn.warehouse_id)
 ORDER BY m.material_code, w.warehouse_code;

\echo '===== ⑦-2 差异行（期望 0 行）====='
WITH inv AS (
  SELECT material_id, warehouse_id, SUM(qty) AS inv_qty FROM biz_inventory GROUP BY 1,2),
txn AS (
  SELECT material_id, warehouse_id,
         SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) AS txn_qty
    FROM biz_inventory_txn GROUP BY 1,2)
SELECT m.material_code, w.warehouse_code, inv.inv_qty, txn.txn_qty,
       inv.inv_qty - txn.txn_qty AS diff
  FROM inv FULL JOIN txn ON inv.material_id=txn.material_id AND inv.warehouse_id=txn.warehouse_id
  JOIN biz_material m ON m.id=COALESCE(inv.material_id,txn.material_id)
  JOIN biz_warehouse w ON w.id=COALESCE(inv.warehouse_id,txn.warehouse_id)
 WHERE COALESCE(inv.inv_qty,0) <> COALESCE(txn.txn_qty,0);

\echo '===== ⑦-3 金额：库存表金额合计 vs 流水入-出金额累计 ====='
SELECT (SELECT COALESCE(SUM(amount),0) FROM biz_inventory)                                  AS inv_amount,
       (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END),0)
          FROM biz_inventory_txn)                                                           AS txn_amount,
       (SELECT COALESCE(SUM(amount),0) FROM biz_inventory)
         - (SELECT COALESCE(SUM(CASE WHEN txn_type='IN' THEN amount ELSE -amount END),0)
              FROM biz_inventory_txn)                                                       AS diff;

\echo '===== ⑦-4 流水自洽：after_qty - before_qty = ±qty（期望 0 行异常）====='
SELECT id, doc_no, txn_type, qty, before_qty, after_qty
  FROM biz_inventory_txn
 WHERE (txn_type='IN'  AND after_qty - before_qty <> qty)
    OR (txn_type='OUT' AND before_qty - after_qty <> qty);

\echo '===== ⑦-5 演练物料逐笔流水（审计轨迹）====='
SELECT m.material_code, w.warehouse_code, t.txn_type, t.biz_type, t.doc_no, t.qty,
       t.before_qty, t.after_qty, t.unit_cost, t.amount, t.period_code
  FROM biz_inventory_txn t
  JOIN biz_material m ON m.id=t.material_id
  JOIN biz_warehouse w ON w.id=t.warehouse_id
 WHERE m.material_code IN ('DEMO-INV01','DEMO-INV02')
 ORDER BY m.material_code, t.created_at, t.doc_no;

\echo '===== ⑧-1 逐 (物料, 仓库)：库位账合计 vs 库存账合计（期望 0 行）====='
WITH cost AS (
  SELECT material_id, warehouse_id, SUM(qty) AS qty FROM biz_inventory GROUP BY 1,2),
loc AS (
  SELECT material_id, warehouse_id, SUM(qty) AS qty FROM biz_inventory_location GROUP BY 1,2)
SELECT m.material_code, w.warehouse_code, cost.qty AS cost_qty, loc.qty AS loc_qty,
       cost.qty - loc.qty AS diff
  FROM cost FULL JOIN loc ON cost.material_id=loc.material_id AND cost.warehouse_id=loc.warehouse_id
  JOIN biz_material m ON m.id=COALESCE(cost.material_id, loc.material_id)
  JOIN biz_warehouse w ON w.id=COALESCE(cost.warehouse_id, loc.warehouse_id)
 WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0);

\echo '===== ⑧-2 逐桶对账：(物料, 仓库, 库位字典) 成本账 vs 库位账（期望 0 行）====='
WITH cost AS (
  SELECT i.material_id, i.warehouse_id, COALESCE(il.location_id::text,'(未分配)') AS bucket,
         SUM(i.qty) AS qty
    FROM biz_inventory i
    LEFT JOIN biz_inventory_location il ON il.id = i.location_id
   GROUP BY 1,2,3),
loc AS (
  SELECT material_id, warehouse_id, COALESCE(location_id::text,'(未分配)') AS bucket,
         SUM(qty) AS qty
    FROM biz_inventory_location GROUP BY 1,2,3)
SELECT m.material_code, w.warehouse_code, COALESCE(cost.bucket, loc.bucket) AS bucket,
       COALESCE(cost.qty,0) AS cost_qty, COALESCE(loc.qty,0) AS loc_qty,
       COALESCE(cost.qty,0) - COALESCE(loc.qty,0) AS diff
  FROM cost FULL JOIN loc
    ON cost.material_id=loc.material_id AND cost.warehouse_id=loc.warehouse_id
   AND cost.bucket=loc.bucket
  JOIN biz_material m ON m.id=COALESCE(cost.material_id, loc.material_id)
  JOIN biz_warehouse w ON w.id=COALESCE(cost.warehouse_id, loc.warehouse_id)
 WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0);

\echo '===== ⑧-3 演练物料的桶明细（人工核对用）====='
SELECT m.material_code, w.warehouse_code,
       COALESCE(l.location_code,'(未分配)') AS bucket,
       i.qty AS cost_qty, i.avg_cost, i.amount, il.qty AS loc_qty
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id
  JOIN biz_warehouse w ON w.id=i.warehouse_id
  LEFT JOIN biz_inventory_location il ON il.id = i.location_id
  LEFT JOIN biz_location l ON l.id = il.location_id
 WHERE m.material_code IN ('DEMO-INV01','DEMO-INV02')
 ORDER BY m.material_code, w.warehouse_code, bucket;

\echo '===== 附加：负库存（期望 0 行）====='
SELECT m.material_code, w.warehouse_code, i.qty
  FROM biz_inventory i
  JOIN biz_material m ON m.id=i.material_id
  JOIN biz_warehouse w ON w.id=i.warehouse_id
 WHERE i.qty < 0;
