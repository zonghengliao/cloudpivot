-- ============================================================================
-- m215_replenish_fixture.sql —— M2-15 补货建议的真接口验收夹具（空库首启后跑）
--
-- 只在**演练库**上跑（库名必须含 cp_/drill/e2e/test）。可重跑：
-- 先按 M215 前缀删干净，再整批插入（不删生产号段、不删种子主数据）。
--
-- 五行格子的设计（每一行都在 m215_replenish_api_check.py 里被断言一次）：
--
--   | 物料 | 仓 | 结存 | 在途 | 未清订单 | 水位 | 期望 |
--   |:--|:--|--:|--:|--:|:--|:--|
--   | M215-F1 | M215-WH | 30 | 5 | 10 | min 50 / max 100 | 预计 35 → 建议 65（PURCHASE，补到最高水位） |
--   | M215-F2 | M215-WH | 30 | 0 | 40 | min 50 | 预计 70 → COVERED（默认不出现，include_covered 才出现） |
--   | M215-F3 | M215-WH | 20 | 0 | 0 | 物料级 safety_stock 40 | min_source=MATERIAL → 建议 20 |
--   | M215-F4 | WH-TRANSIT（虚拟在途仓） | 0 | 0 | 0 | min 100 | **不出现**（在途仓不可补货） |
--   | M215-F5 | M215-WH | 50 | 0 | 0 | min 50 | **不出现**（预计可用 == 最低水位，边界不触发） |
-- ============================================================================

\set ON_ERROR_STOP on

BEGIN;

-- ---------- ① 清场（只删 M215 自己的行；顺序受外键约束）----------
DELETE FROM biz_inventory_in_transit
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M215-%');
DELETE FROM biz_shipment WHERE ship_no LIKE 'M215-%';
DELETE FROM biz_sales_order WHERE so_no LIKE 'M215-%';
DELETE FROM biz_stock_in WHERE in_no LIKE 'M215-%';
DELETE FROM biz_purchase_order_item
 WHERE order_id IN (SELECT id FROM biz_purchase_order WHERE order_no LIKE 'M215-%');
DELETE FROM biz_purchase_order WHERE order_no LIKE 'M215-%';
DELETE FROM biz_inventory
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M215-%');
DELETE FROM biz_inventory_location
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M215-%');
DELETE FROM biz_material_stock_policy
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M215-%');
DELETE FROM biz_material WHERE material_code LIKE 'M215-%';
DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'M215-%';
DELETE FROM biz_unit WHERE unit_code LIKE 'M215-%';
DELETE FROM biz_supplier WHERE supplier_code LIKE 'M215-%';
DELETE FROM biz_customer WHERE code LIKE 'M215-%';

-- ---------- ② 主数据 ----------
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status)
VALUES ('a2150000-0000-4000-8000-000000000001', 'M215-PCS', '件(M215)', 1, 1);

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, warehouse_type,
                           is_leaf, level, status)
VALUES ('a2150000-0000-4000-8000-000000000010', 'M215-WH', '补货建议仓', '/M215-WH', 'NORMAL', 1, 1, 1);

INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
VALUES ('a2150000-0000-4000-8000-000000000020', 'M215-SUP', '补货建议供应商', 1);

INSERT INTO biz_customer (id, code, name, status)
VALUES ('a2150000-0000-4000-8000-000000000030', 'M215-CUS', '补货建议客户', 'ACTIVE');

INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                          safety_stock, status)
VALUES
  ('a2150000-0000-4000-8000-000000000101', 'M215-F1', '补货建议物料一', 'SPEC-1',
   'a2150000-0000-4000-8000-000000000001', 'RAW', 0, 1),
  ('a2150000-0000-4000-8000-000000000102', 'M215-F2', '补货建议物料二', 'SPEC-2',
   'a2150000-0000-4000-8000-000000000001', 'RAW', 0, 1),
  ('a2150000-0000-4000-8000-000000000103', 'M215-F3', '补货建议物料三', 'SPEC-3',
   'a2150000-0000-4000-8000-000000000001', 'RAW', 40, 1),   -- 物料级安全库存 40
  ('a2150000-0000-4000-8000-000000000104', 'M215-F4', '补货建议物料四', 'SPEC-4',
   'a2150000-0000-4000-8000-000000000001', 'RAW', 0, 1),
  ('a2150000-0000-4000-8000-000000000105', 'M215-F5', '补货建议物料五', 'SPEC-5',
   'a2150000-0000-4000-8000-000000000001', 'RAW', 0, 1);

-- ---------- ③ 水位设置 ----------
INSERT INTO biz_material_stock_policy (material_id, warehouse_id, min_qty, max_qty, status)
VALUES
  ('a2150000-0000-4000-8000-000000000101', 'a2150000-0000-4000-8000-000000000010', 50, 100, 1),
  ('a2150000-0000-4000-8000-000000000102', 'a2150000-0000-4000-8000-000000000010', 50, NULL, 1),
  ('a2150000-0000-4000-8000-000000000104',
   (SELECT id FROM biz_warehouse WHERE warehouse_code = 'WH-TRANSIT' AND deleted_at IS NULL LIMIT 1),
   100, NULL, 1),
  ('a2150000-0000-4000-8000-000000000105', 'a2150000-0000-4000-8000-000000000010', 50, NULL, 1);

-- ---------- ④ 结存 ----------
INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount)
VALUES
  (gen_random_uuid(), 'a2150000-0000-4000-8000-000000000101', 'a2150000-0000-4000-8000-000000000010', 30, 0, 10, 300),
  (gen_random_uuid(), 'a2150000-0000-4000-8000-000000000102', 'a2150000-0000-4000-8000-000000000010', 30, 0, 10, 300),
  (gen_random_uuid(), 'a2150000-0000-4000-8000-000000000103', 'a2150000-0000-4000-8000-000000000010', 20, 0, 10, 200),
  (gen_random_uuid(), 'a2150000-0000-4000-8000-000000000105', 'a2150000-0000-4000-8000-000000000010', 50, 0, 10, 500);

-- 在途仓那一格（F4）：有结存、有水位，但仓类型是 IN_TRANSIT → 必须被排除
INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount)
SELECT gen_random_uuid(), 'a2150000-0000-4000-8000-000000000104', w.id, 0, 0, 0, 0
  FROM biz_warehouse w
 WHERE w.warehouse_code = 'WH-TRANSIT' AND w.deleted_at IS NULL;

-- ---------- ⑤ 发货在途（F1：5）----------
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status)
VALUES ('a2150000-0000-4000-8000-000000000201', 'M215-SO-0001',
        'a2150000-0000-4000-8000-000000000030', CURRENT_DATE, 'APPROVED');

INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status)
VALUES ('a2150000-0000-4000-8000-000000000202', 'M215-SH-0001',
        'a2150000-0000-4000-8000-000000000201',
        'a2150000-0000-4000-8000-000000000030',
        'a2150000-0000-4000-8000-000000000010', CURRENT_DATE, 'POSTED');

INSERT INTO biz_inventory_in_transit (shipment_id, material_id, warehouse_id, ship_date,
                                      shipped_qty, received_qty, status)
VALUES ('a2150000-0000-4000-8000-000000000202',
        'a2150000-0000-4000-8000-000000000101',
        'a2150000-0000-4000-8000-000000000010', CURRENT_DATE, 5, 0, 'IN_TRANSIT');

-- ---------- ⑥ 未清采购订单 ----------
-- F1：已审批 10 未收 → 计入
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status)
VALUES ('a2150000-0000-4000-8000-000000000301', 'M215-PO-0001',
        'a2150000-0000-4000-8000-000000000020', CURRENT_DATE,
        'a2150000-0000-4000-8000-000000000010', 'APPROVED');
INSERT INTO biz_purchase_order_item (order_id, line_no, material_id, unit_id, qty, price)
VALUES ('a2150000-0000-4000-8000-000000000301', 1,
        'a2150000-0000-4000-8000-000000000101',
        'a2150000-0000-4000-8000-000000000001', 10, 10);

-- F1：草稿 100（不算，否则建议会被假供给掩盖）
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status)
VALUES ('a2150000-0000-4000-8000-000000000302', 'M215-PO-0002',
        'a2150000-0000-4000-8000-000000000020', CURRENT_DATE,
        'a2150000-0000-4000-8000-000000000010', 'DRAFT');
INSERT INTO biz_purchase_order_item (order_id, line_no, material_id, unit_id, qty, price)
VALUES ('a2150000-0000-4000-8000-000000000302', 1,
        'a2150000-0000-4000-8000-000000000101',
        'a2150000-0000-4000-8000-000000000001', 100, 10);

-- F2：部分到货（订购 50、已过账收货 10 → 未收 40）→ 计入（把 30 的账面抬到 70，覆盖 50 的水位）
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status)
VALUES ('a2150000-0000-4000-8000-000000000303', 'M215-PO-0003',
        'a2150000-0000-4000-8000-000000000020', CURRENT_DATE,
        'a2150000-0000-4000-8000-000000000010', 'PARTIAL_RECEIVED');
INSERT INTO biz_purchase_order_item (id, order_id, line_no, material_id, unit_id, qty, price)
VALUES ('a2150000-0000-4000-8000-000000000313', 'a2150000-0000-4000-8000-000000000303', 1,
        'a2150000-0000-4000-8000-000000000102',
        'a2150000-0000-4000-8000-000000000001', 50, 10);
-- 已收数量没有列（V0217 / P2-3 删了 received_qty）：只算**已过账收货单**，
-- 所以夹具必须真的过账一张收货单（10），否则「已收」就是 0。
INSERT INTO biz_stock_in (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status, total_qty)
VALUES ('a2150000-0000-4000-8000-000000000401', 'M215-IN-0001', 'PURCHASE',
        'a2150000-0000-4000-8000-000000000303', 'a2150000-0000-4000-8000-000000000020',
        'a2150000-0000-4000-8000-000000000010', CURRENT_DATE, 'POSTED', 10);
INSERT INTO biz_stock_in_item (stock_in_id, line_no, source_item_id, material_id, unit_id, qty)
VALUES ('a2150000-0000-4000-8000-000000000401', 1,
        'a2150000-0000-4000-8000-000000000313',
        'a2150000-0000-4000-8000-000000000102',
        'a2150000-0000-4000-8000-000000000001', 10);

COMMIT;

-- ---------- ⑦ 夹具自检（数字与上面表格逐个对上）----------
DO $$
DECLARE
    n_mat  integer;
    n_inv  integer;
    n_pol  integer;
    n_po   integer;
    n_tt   integer;
BEGIN
    SELECT count(*) INTO n_mat FROM biz_material WHERE material_code LIKE 'M215-%' AND deleted_at IS NULL;
    SELECT count(*) INTO n_inv FROM biz_inventory
     WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M215-%');
    SELECT count(*) INTO n_pol FROM biz_material_stock_policy
     WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M215-%');
    SELECT count(*) INTO n_po FROM biz_purchase_order WHERE order_no LIKE 'M215-%' AND deleted_at IS NULL;
    SELECT count(*) INTO n_tt FROM biz_inventory_in_transit
     WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M215-%')
       AND status = 'IN_TRANSIT';
    IF n_mat <> 5 OR n_inv <> 5 OR n_pol <> 4 OR n_po <> 3 OR n_tt <> 1 THEN
        RAISE EXCEPTION 'M215 夹具自检失败：物料 %/5、库存行 %/5、水位 %/4、采购单 %/3、在途 %/1',
            n_mat, n_inv, n_pol, n_po, n_tt;
    END IF;
    RAISE NOTICE 'M215 夹具就绪：5 物料 / 5 库存行 / 4 水位 / 3 采购单 / 1 在途';
END $$;
