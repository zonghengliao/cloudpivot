-- COST-B1-12 前端验收夹具（一次性，只在本机 e2e 库 b112e2e 跑）
-- 全部固定 UUID 前缀 b1120000-*，可按前缀一键清除。
BEGIN;

-- 1) 物料：一个原料（有批次）、一个产成品（成本对象的产品）
INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status, serial_required, created_at, updated_at)
SELECT 'b1120000-0000-4000-8000-000000000011'::uuid, 'B112-M1', 'B112 钢板', u.id,
       'RAW', 1, false, now(), now()
  FROM biz_unit u WHERE u.deleted_at IS NULL ORDER BY u.unit_code LIMIT 1
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status, serial_required, created_at, updated_at)
SELECT 'b1120000-0000-4000-8000-000000000021'::uuid, 'B112-P1', 'B112 产成品', u.id,
       'FINISHED', 1, false, now(), now()
  FROM biz_unit u WHERE u.deleted_at IS NULL ORDER BY u.unit_code LIMIT 1
ON CONFLICT (id) DO NOTHING;

-- 2) 仓库
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative)
VALUES ('b1120000-0000-4000-8000-0000000000f1'::uuid, 'B112-WH', 'B112 车间仓',
        '/b1120000-0000-4000-8000-0000000000f1/', 1, 1, 1, 0)
ON CONFLICT (id) DO NOTHING;

-- 3) 批次：两条原料批（一条合格、一条待检）+ 一条产成品批
INSERT INTO biz_lot (id, lot_no, material_id, source_type, source_doc_type, source_doc_no, prod_date, expire_date, qc_status, status, remark)
VALUES
  ('b1120000-0000-4000-8000-0000000000b1'::uuid, 'LOT-B112-A', 'b1120000-0000-4000-8000-000000000011'::uuid,
   'PURCHASE', 'STOCK_IN', 'IN-B112-000001', '2026-11-01', '2027-11-01', 'PASSED', 1, 'B1-12 验收：合格批'),
  ('b1120000-0000-4000-8000-0000000000b2'::uuid, 'LOT-B112-B', 'b1120000-0000-4000-8000-000000000011'::uuid,
   'PURCHASE', 'STOCK_IN', 'IN-B112-000002', '2026-11-05', NULL, 'PENDING', 1, 'B1-12 验收：待检批'),
  ('b1120000-0000-4000-8000-0000000000b3'::uuid, 'LOT-B112-P1', 'b1120000-0000-4000-8000-000000000021'::uuid,
   'PRODUCTION', 'COST_OBJECT', NULL, '2026-11-06', NULL, 'NA', 0, 'B1-12 验收：停用批（下拉里不该出现）')
ON CONFLICT (id) DO NOTHING;

-- 4) 成本对象：一个 OPEN（下拉里应有）、一个 CLOSED（下拉里不该有）
INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, target_warehouse_id, plan_qty,
                             workshop_dept_id, workshop_dept_name, start_date, status, closed_at, created_at, updated_at)
SELECT 'b1120000-0000-4000-8000-0000000000a1'::uuid, 'CB-B112-000001', 'BATCH',
       'b1120000-0000-4000-8000-000000000021'::uuid, 'LOT-B112-P1',
       'b1120000-0000-4000-8000-0000000000f1'::uuid, 100,
       d.id, COALESCE(d.dept_name, 'B112 车间'), '2026-11-01', 'OPEN', NULL, now(), now()
  FROM (SELECT id, dept_name FROM sys_dept WHERE deleted_at IS NULL ORDER BY sort_order LIMIT 1) d
ON CONFLICT (id) DO NOTHING;

INSERT INTO biz_cost_object (id, object_no, object_type, product_id, lot_no, target_warehouse_id, plan_qty,
                             workshop_dept_id, workshop_dept_name, start_date, status, closed_at, created_at, updated_at)
SELECT 'b1120000-0000-4000-8000-0000000000a2'::uuid, 'CB-B112-000002', 'BATCH',
       'b1120000-0000-4000-8000-000000000021'::uuid, 'LOT-B112-P1',
       'b1120000-0000-4000-8000-0000000000f1'::uuid, 50,
       d.id, COALESCE(d.dept_name, 'B112 车间'), '2026-10-01', 'CLOSED', now(), now(), now()
  FROM (SELECT id, dept_name FROM sys_dept WHERE deleted_at IS NULL ORDER BY sort_order LIMIT 1) d
ON CONFLICT (id) DO NOTHING;

COMMIT;

SELECT 'materials' AS what, count(*) FROM biz_material WHERE id IN
  ('b1120000-0000-4000-8000-000000000011'::uuid, 'b1120000-0000-4000-8000-000000000021'::uuid)
UNION ALL
SELECT 'lots', count(*) FROM biz_lot WHERE id IN
  ('b1120000-0000-4000-8000-0000000000b1'::uuid, 'b1120000-0000-4000-8000-0000000000b2'::uuid,
   'b1120000-0000-4000-8000-0000000000b3'::uuid)
UNION ALL
SELECT 'cost_objects', count(*) FROM biz_cost_object WHERE id IN
  ('b1120000-0000-4000-8000-0000000000a1'::uuid, 'b1120000-0000-4000-8000-0000000000a2'::uuid);
