\echo '--- M1@A 成本账桶 ---'
SELECT i.id, i.location_id, il.location_id AS dict, i.qty, i.avg_cost, i.amount
  FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id
  JOIN biz_material m ON m.id=i.material_id
 WHERE m.material_code='E2E03-M1' ORDER BY i.warehouse_id, i.location_id;
\echo '--- M1 库位账 ---'
SELECT w.warehouse_code, l.location_code, il.qty
  FROM biz_inventory_location il
  JOIN biz_material m ON m.id=il.material_id
  JOIN biz_warehouse w ON w.id=il.warehouse_id
  LEFT JOIN biz_location l ON l.id=il.location_id
 WHERE m.material_code='E2E03-M1' ORDER BY w.warehouse_code, l.location_code;
\echo '--- 调拨流水金额 ---'
SELECT txn_type, biz_type, doc_no, qty, unit_cost, amount FROM biz_inventory_txn t JOIN biz_material m ON m.id=t.material_id
 WHERE m.material_code='E2E03-M1' ORDER BY t.created_at;
