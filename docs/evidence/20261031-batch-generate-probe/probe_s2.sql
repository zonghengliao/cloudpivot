-- ADR 规格取证：序列号「两本账」在**生成后第一次出库**时会不会自己变红
-- 用法：psql -d arch_spec_probe -f probe.sql
-- 全部在事务里跑，最后 ROLLBACK，不污染库。

\set ON_ERROR_STOP on
BEGIN;

-- ---------- 夹具 ----------
INSERT INTO biz_unit (id, unit_code, unit_name) VALUES
  ('11111111-1111-4111-8111-111111111111', 'PCS-P', '件') ON CONFLICT DO NOTHING;

INSERT INTO biz_material (id, material_code, material_name, unit_id, serial_required)
VALUES ('a0000000-0000-4000-8000-00000000a001', 'PROBE-M1', '取证物料', '11111111-1111-4111-8111-111111111111', true)
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path)
VALUES ('b0000000-0000-4000-8000-00000000b001', 'PROBE-W1', '取证仓', '/PROBE-W1')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_location (id, warehouse_id, location_code, location_name)
VALUES ('c0000000-0000-4000-8000-00000000c001','b0000000-0000-4000-8000-00000000b001','PROBE-L1','取证库位')
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty)
VALUES ('d0000000-0000-4000-8000-00000000d001','a0000000-0000-4000-8000-00000000a001',
        'b0000000-0000-4000-8000-00000000b001','c0000000-0000-4000-8000-00000000c001',0,0)
ON CONFLICT DO NOTHING;

-- ============================================================
-- 场景 A：按 ADR 原文（有 location_id 就落库位行）
-- ============================================================
-- A1 生成 5 件：成本账（apply_cost 加法口径）
INSERT INTO biz_inventory AS i
    (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at)
VALUES (gen_random_uuid(),'a0000000-0000-4000-8000-00000000a001',
        'b0000000-0000-4000-8000-00000000b001','d0000000-0000-4000-8000-00000000d001',
        5, 0, round(50.00/5,6), 50.00, now())
ON CONFLICT (material_id, warehouse_id, COALESCE(location_id,'00000000-0000-0000-0000-000000000000'::uuid))
DO UPDATE SET qty = i.qty + EXCLUDED.qty, amount = i.amount + EXCLUDED.amount,
              avg_cost = round((i.amount+EXCLUDED.amount)/(i.qty+EXCLUDED.qty),6);

-- A2 生成 5 件：库位账（apply_location_qty 分支 A）
UPDATE biz_inventory_location SET qty = qty + 5
 WHERE id = 'd0000000-0000-4000-8000-00000000d001';

-- A3 落 5 个号（batch_generate 的 INSERT … SELECT 形态，location_id = 库位行 id）
INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, location_id, status, inbound_at)
SELECT gen_random_uuid(), 'SN-PROBE-' || g, 'a0000000-0000-4000-8000-00000000a001',
       'b0000000-0000-4000-8000-00000000b001', 'd0000000-0000-4000-8000-00000000d001',
       'IN_STOCK', now()
  FROM generate_series(1,5) g;

\echo '=== A 生成后 ==='
SELECT 'A:成本账sum' k, sum(qty) v FROM biz_inventory
 WHERE material_id='a0000000-0000-4000-8000-00000000a001'
UNION ALL SELECT 'A:库位账sum', sum(qty) FROM biz_inventory_location
 WHERE material_id='a0000000-0000-4000-8000-00000000a001'
UNION ALL SELECT 'A:在库号数', count(*) FROM biz_material_serial
 WHERE material_id='a0000000-0000-4000-8000-00000000a001' AND status='IN_STOCK';

-- A4 出库 1 件（transition_status 把状态改成 OUTBOUND）
UPDATE biz_material_serial SET status='OUTBOUND', outbound_at=now()
 WHERE id = (SELECT id FROM biz_material_serial
              WHERE material_id='a0000000-0000-4000-8000-00000000a001' AND status='IN_STOCK' LIMIT 1);

-- A5 出库走 link_inventory → recompute_quantity（原文照抄 serial_model.rs:592-629）
INSERT INTO biz_inventory AS i
    (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount, last_txn_at)
SELECT gen_random_uuid(), 'a0000000-0000-4000-8000-00000000a001'::uuid,
       'b0000000-0000-4000-8000-00000000b001'::uuid, s.in_stock, 0, 0, 0, now()
  FROM (SELECT COUNT(*)::numeric AS in_stock FROM biz_material_serial
         WHERE material_id='a0000000-0000-4000-8000-00000000a001'::uuid
           AND warehouse_id='b0000000-0000-4000-8000-00000000b001'::uuid
           AND status='IN_STOCK' AND deleted_at IS NULL) s
ON CONFLICT (material_id, warehouse_id, COALESCE(location_id,'00000000-0000-0000-0000-000000000000'::uuid))
DO UPDATE SET qty = EXCLUDED.qty, amount = EXCLUDED.qty * i.avg_cost, last_txn_at = now(),
              updated_at = now();

\echo '=== A 出库 1 件后（关键：B1/C5 会不会红） ==='
SELECT 'A:成本账sum' k, sum(qty) v FROM biz_inventory
 WHERE material_id='a0000000-0000-4000-8000-00000000a001'
UNION ALL SELECT 'A:库位账sum', sum(qty) FROM biz_inventory_location
 WHERE material_id='a0000000-0000-4000-8000-00000000a001'
UNION ALL SELECT 'A:在库号数', count(*) FROM biz_material_serial
 WHERE material_id='a0000000-0000-4000-8000-00000000a001' AND status='IN_STOCK';

ROLLBACK;
