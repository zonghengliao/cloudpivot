\echo '--- MS 成本账 ---'
SELECT i.material_id, i.warehouse_id, i.location_id, i.qty, i.amount, i.avg_cost, i.created_at
  FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id WHERE m.material_code='E2E03-MS';
\echo '--- MS 库位账 ---'
SELECT il.id, il.material_id, il.warehouse_id, il.location_id, il.qty, il.amount FROM biz_inventory_location il
  JOIN biz_material m ON m.id=il.material_id WHERE m.material_code='E2E03-MS';
\echo '--- MS 流水 ---'
SELECT txn_type, biz_type, doc_no, qty, before_qty, after_qty, unit_cost, amount, period_code
  FROM biz_inventory_txn t JOIN biz_material m ON m.id=t.material_id WHERE m.material_code='E2E03-MS' ORDER BY t.created_at;
\echo '--- MS 序列号 ---'
SELECT serial_no, status, warehouse_id, location_id, unit_cost, source_type, source_id IS NOT NULL AS has_source
  FROM biz_material_serial s JOIN biz_material m ON m.id=s.material_id WHERE m.material_code='E2E03-MS' ORDER BY created_at;
\echo '--- 入库单 ---'
SELECT id, in_no, status, in_type, total_amount, approval_instance_id, remark FROM biz_stock_in WHERE remark LIKE 'E2E-03%' ORDER BY created_at;
