-- P0-03：修复前那笔调拨（TK-20261107-000001）在 DEMO-INV01 / WH-DVA 上留下的
-- 分库位漂移（成本账：库位桶 50 / 汇总桶 42；库位账：库位桶 10 / 汇总桶 82）。
-- 只修**本次演练自己造的那只物料**、只动 WH-DVA 的两条库位账行，按成本账口径把
-- 40 件从「未分配桶」挪回「LOC-DVA-01 桶」。不动任何别的物料 / 仓库 / 单据。
\echo '===== 修复前：库存账 vs 库位账（DEMO-INV01，WH-DVA）====='
SELECT COALESCE(il.location_id::text,'(未分配)') AS bucket,
       i.qty AS cost_qty, il.qty AS loc_qty, i.qty - COALESCE(il.qty,0) AS diff
  FROM biz_inventory i
  LEFT JOIN biz_inventory_location il ON il.id = i.location_id
 WHERE i.material_id = (SELECT id FROM biz_material WHERE material_code='DEMO-INV01')
   AND i.warehouse_id = (SELECT id FROM biz_warehouse WHERE warehouse_code='WH-DVA')
 ORDER BY 1;

BEGIN;
-- 未分配库位桶：82 → 42（-40）
UPDATE biz_inventory_location
   SET qty = qty - 40, updated_at = now()
 WHERE material_id = (SELECT id FROM biz_material WHERE material_code='DEMO-INV01')
   AND warehouse_id = (SELECT id FROM biz_warehouse WHERE warehouse_code='WH-DVA')
   AND location_id IS NULL
   AND qty = 82;
-- 具体库位桶（LOC-DVA-01）：10 → 50（+40）
UPDATE biz_inventory_location il
   SET qty = il.qty + 40, updated_at = now()
  FROM biz_location l
 WHERE l.id = il.location_id AND l.location_code = 'LOC-DVA-01'
   AND il.material_id = (SELECT id FROM biz_material WHERE material_code='DEMO-INV01')
   AND il.warehouse_id = (SELECT id FROM biz_warehouse WHERE warehouse_code='WH-DVA')
   AND il.qty = 10;
COMMIT;

\echo '===== 修复后：库存账 vs 库位账（DEMO-INV01，WH-DVA）====='
SELECT COALESCE(il.location_id::text,'(未分配)') AS bucket,
       i.qty AS cost_qty, il.qty AS loc_qty, i.qty - COALESCE(il.qty,0) AS diff
  FROM biz_inventory i
  LEFT JOIN biz_inventory_location il ON il.id = i.location_id
 WHERE i.material_id = (SELECT id FROM biz_material WHERE material_code='DEMO-INV01')
   AND i.warehouse_id = (SELECT id FROM biz_warehouse WHERE warehouse_code='WH-DVA')
 ORDER BY 1;
