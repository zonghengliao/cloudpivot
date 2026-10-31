-- ADR 规格取证 ②：S1（成本账/库位账都落 location_id IS NULL 汇总桶）在出库后是否自洽
-- B 段 = 出库只改成本账（现状代码）→ B1 红
-- C 段 = 出库同时按件数重算库位账（本规格新增的一步）→ B1/C5 全绿
\set ON_ERROR_STOP on
BEGIN;

INSERT INTO biz_unit (id, unit_code, unit_name) VALUES
  ('22222222-2222-4222-8222-222222222222', 'PCS-Q', '件') ON CONFLICT DO NOTHING;
INSERT INTO biz_material (id, material_code, material_name, unit_id, serial_required)
VALUES ('a0000000-0000-4000-8000-00000000a002', 'PROBE-M2', '取证物料2', '22222222-2222-4222-8222-222222222222', true)
ON CONFLICT (id) DO NOTHING;
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path)
VALUES ('b0000000-0000-4000-8000-00000000b002', 'PROBE-W2', '取证仓2', '/PROBE-W2') ON CONFLICT DO NOTHING;

-- 生成 5 件：两本账都落 NULL 汇总桶
INSERT INTO biz_inventory AS i
    (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at)
VALUES (gen_random_uuid(),'a0000000-0000-4000-8000-00000000a002',
        'b0000000-0000-4000-8000-00000000b002', NULL, 5, 0, round(50.00/5,6), 50.00, now())
ON CONFLICT (material_id, warehouse_id, COALESCE(location_id,'00000000-0000-0000-0000-000000000000'::uuid))
DO UPDATE SET qty = i.qty + EXCLUDED.qty, amount = i.amount + EXCLUDED.amount,
              avg_cost = round((i.amount+EXCLUDED.amount)/(i.qty+EXCLUDED.qty),6);

INSERT INTO biz_inventory_location AS il (id, material_id, warehouse_id, location_id, qty, locked_qty)
VALUES (gen_random_uuid(),'a0000000-0000-4000-8000-00000000a002','b0000000-0000-4000-8000-00000000b002',NULL,5,0)
ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET qty = il.qty + EXCLUDED.qty;

INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status, inbound_at)
SELECT gen_random_uuid(), 'SN-PB2-' || g, 'a0000000-0000-4000-8000-00000000a002',
       'b0000000-0000-4000-8000-00000000b002', 'IN_STOCK', now() FROM generate_series(1,5) g;

-- 出库 1 件 + 现状口径的 recompute_quantity（只改成本账）
UPDATE biz_material_serial SET status='OUTBOUND', outbound_at=now()
 WHERE id = (SELECT id FROM biz_material_serial
              WHERE material_id='a0000000-0000-4000-8000-00000000a002' AND status='IN_STOCK' LIMIT 1);

INSERT INTO biz_inventory AS i
    (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount, last_txn_at)
SELECT gen_random_uuid(),'a0000000-0000-4000-8000-00000000a002'::uuid,
       'b0000000-0000-4000-8000-00000000b002'::uuid, s.in_stock, 0, 0, 0, now()
  FROM (SELECT COUNT(*)::numeric AS in_stock FROM biz_material_serial
         WHERE material_id='a0000000-0000-4000-8000-00000000a002'::uuid
           AND status='IN_STOCK' AND deleted_at IS NULL) s
ON CONFLICT (material_id, warehouse_id, COALESCE(location_id,'00000000-0000-0000-0000-000000000000'::uuid))
DO UPDATE SET qty = EXCLUDED.qty, amount = EXCLUDED.qty * i.avg_cost, last_txn_at=now(), updated_at=now();

\echo '=== B 出库 1 件后（现状代码：库位账不动） ==='
SELECT 'B:成本账sum' k, sum(qty) v FROM biz_inventory
 WHERE material_id='a0000000-0000-4000-8000-00000000a002'
UNION ALL SELECT 'B:库位账sum', sum(qty) FROM biz_inventory_location
 WHERE material_id='a0000000-0000-4000-8000-00000000a002'
UNION ALL SELECT 'B:在库号数', count(*) FROM biz_material_serial
 WHERE material_id='a0000000-0000-4000-8000-00000000a002' AND status='IN_STOCK';

-- C：出库时同步按件数重算库位账（NULL 汇总桶，与成本账同一把尺子）
INSERT INTO biz_inventory_location AS il (id, material_id, warehouse_id, location_id, qty, locked_qty)
SELECT gen_random_uuid(),'a0000000-0000-4000-8000-00000000a002'::uuid,
       'b0000000-0000-4000-8000-00000000b002'::uuid, NULL, s.in_stock, 0
  FROM (SELECT COUNT(*)::numeric AS in_stock FROM biz_material_serial
         WHERE material_id='a0000000-0000-4000-8000-00000000a002'::uuid
           AND status='IN_STOCK' AND deleted_at IS NULL) s
ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET qty = EXCLUDED.qty, updated_at = now();

\echo '=== C 出库 1 件 + 库位账重算后（本规格新增一步） ==='
SELECT 'C:成本账sum' k, sum(qty) v FROM biz_inventory
 WHERE material_id='a0000000-0000-4000-8000-00000000a002'
UNION ALL SELECT 'C:库位账sum', sum(qty) FROM biz_inventory_location
 WHERE material_id='a0000000-0000-4000-8000-00000000a002'
UNION ALL SELECT 'C:在库号数', count(*) FROM biz_material_serial
 WHERE material_id='a0000000-0000-4000-8000-00000000a002' AND status='IN_STOCK'
UNION ALL SELECT 'C:avg_cost', max(avg_cost) FROM biz_inventory
 WHERE material_id='a0000000-0000-4000-8000-00000000a002';

ROLLBACK;
