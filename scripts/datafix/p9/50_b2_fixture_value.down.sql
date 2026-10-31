-- P9 / B2 第三笔回滚：把 20 元的夹具金额改回 biz_inventory（仅当它仍是 0 时）
UPDATE biz_inventory inv
   SET amount = 20,
       avg_cost = 10,
       updated_at = now()
 WHERE inv.material_id = (SELECT m.id FROM biz_material m
                           WHERE m.material_code = 'M-W1A-004' AND m.deleted_at IS NULL
                           ORDER BY m.created_at LIMIT 1)
   AND inv.warehouse_id = (SELECT w.id FROM biz_warehouse w
                            WHERE w.warehouse_code = 'WH01'
                            ORDER BY w.created_at LIMIT 1)
   AND inv.location_id IS NULL
   AND inv.qty = 2
   AND inv.amount = 0;
