-- FIX-4 / V0324 存量数据迁移用例（造一段「期初落未分配桶 + 收货落默认库位桶」的老数据）
-- 用法：psql ... -f 20_seed_legacy.sql  →  再跑 V0324 up.sql  →  再跑 21_assert.sql
-- 全部固定 UUID，重复执行先清后建（幂等）

\set ON_ERROR_STOP on

-- 清理上一轮
DELETE FROM biz_opening_inventory WHERE period_code = '2033-11'
    AND material_id = 'f4f40002-0000-4000-8000-000000000001';
DELETE FROM biz_inventory_txn WHERE material_id = 'f4f40002-0000-4000-8000-000000000001';
DELETE FROM biz_inventory WHERE material_id = 'f4f40002-0000-4000-8000-000000000001';
DELETE FROM biz_inventory_location WHERE material_id = 'f4f40002-0000-4000-8000-000000000001';
DELETE FROM biz_material WHERE id = 'f4f40002-0000-4000-8000-000000000001';
DELETE FROM biz_location WHERE id = 'f4f40003-0000-4000-8000-0000000000d1';
DELETE FROM biz_warehouse WHERE id = 'f4f40001-0000-4000-8000-000000000001';

-- 仓库 + 默认库位
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, allow_negative,
                           status, sort_order, parent_id, level, warehouse_path, is_leaf)
VALUES ('f4f40001-0000-4000-8000-000000000001', 'WH-F4-LEGACY', 'F4 存量仓', 'NORMAL', 0,
        1, 1, NULL, 1, '/f4legacy/', 1);

INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                          is_default, capacity, status, sort_order)
VALUES ('f4f40003-0000-4000-8000-0000000000d1', 'F4-L-DEFAULT', 'F4 默认库位',
        'f4f40001-0000-4000-8000-000000000001', 'STORAGE', 1, NULL, 1, 1);

-- 物料
INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type,
                          safety_stock, costing_method, tax_rate, status, serial_required)
SELECT 'f4f40002-0000-4000-8000-000000000001', 'F4-M-LEGACY', 'F4 存量物料', u.id, 'RAW',
       0, 'MOVING_AVG', 0, 1, false
  FROM (SELECT id FROM biz_unit LIMIT 1) u;

-- 老数据形态：同仓两桶并存
--   ① 未分配桶：期初落的 40 件（成本账 40 / 400）+ 库位账 40
--   ② 默认库位桶：收货落的 10 件（成本账 10 / 100）+ 库位账 10
INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty)
VALUES ('f4f40004-0000-4000-8000-0000000000a1', 'f4f40002-0000-4000-8000-000000000001',
        'f4f40001-0000-4000-8000-000000000001', NULL, 40, 0),
       ('f4f40004-0000-4000-8000-0000000000a2', 'f4f40002-0000-4000-8000-000000000001',
        'f4f40001-0000-4000-8000-000000000001', 'f4f40003-0000-4000-8000-0000000000d1', 10, 0);

INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty,
                           avg_cost, amount, last_txn_at)
VALUES (gen_random_uuid(), 'f4f40002-0000-4000-8000-000000000001',
        'f4f40001-0000-4000-8000-000000000001', NULL, 40, 0, 10, 400, now()),
       (gen_random_uuid(), 'f4f40002-0000-4000-8000-000000000001',
        'f4f40001-0000-4000-8000-000000000001',
        'f4f40004-0000-4000-8000-0000000000a2', 10, 0, 10, 100, now());

-- 期初台账（V0324 之前的行：没有 location_row_id）
INSERT INTO biz_opening_inventory (id, period_code, material_id, warehouse_id, qty, unit_cost,
                                   amount, remark)
VALUES (gen_random_uuid(), '2033-11', 'f4f40002-0000-4000-8000-000000000001',
        'f4f40001-0000-4000-8000-000000000001', 40, 10, 400, 'F4 存量期初');

-- 序列号 + 收货/调拨明细指向「未分配桶」的库位账行（迁移必须把这些引用改指向目标行，否则 FK 挡着删不掉）
INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, location_id, status)
VALUES ('f4f40005-0000-4000-8000-000000000001', 'F4-SN-LEGACY-1',
        'f4f40002-0000-4000-8000-000000000001', 'f4f40001-0000-4000-8000-000000000001',
        'f4f40004-0000-4000-8000-0000000000a1', 'IN_STOCK');

-- 库存流水（无库位列）：40（期初）+ 10（收货）= 50，保证「库存表 = 流水累计」对得上
INSERT INTO biz_inventory_txn (id, material_id, warehouse_id, txn_type, biz_type, doc_type,
                               doc_id, doc_no, line_no, qty, unit_cost, amount,
                               before_qty, before_amount, before_cost,
                               after_qty, after_amount, after_cost, period_code, remark)
VALUES (gen_random_uuid(), 'f4f40002-0000-4000-8000-000000000001',
        'f4f40001-0000-4000-8000-000000000001', 'IN', 'OPENING', 'OPENING',
        gen_random_uuid(), 'OPENING-2033-11', 1, 40, 10, 400,
        0, 0, 0, 40, 400, 10, '2033-11', 'F4 存量期初'),
       (gen_random_uuid(), 'f4f40002-0000-4000-8000-000000000001',
        'f4f40001-0000-4000-8000-000000000001', 'IN', 'PURCHASE_IN', 'STOCK_IN',
        gen_random_uuid(), 'F4-IN-0001', 1, 10, 10, 100,
        40, 400, 10, 50, 500, 10, '2033-11', 'F4 存量收货');
