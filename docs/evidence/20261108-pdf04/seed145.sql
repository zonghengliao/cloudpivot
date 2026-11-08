-- PDF-04 @145 隔离实例夹具（库 cloudpivot_pdf04，命名空间 0f51…0f55）
-- 目的：给四张单据（调拨单 / 盘点单 / 采购退货单 / 销售退货单）造真实数据，供真实 token 导出实测。
-- 全部固定 UUID；先删后插（幂等），DELETE 顺序按外键依赖倒序。
\set ON_ERROR_STOP on

BEGIN;

-- ============================ 清场 ============================
DELETE FROM biz_stock_transfer_item_serial WHERE item_id IN
  (SELECT id FROM biz_stock_transfer_item WHERE transfer_id = '0f520000-0000-4000-8000-000000000001');
DELETE FROM biz_stock_transfer_item WHERE transfer_id = '0f520000-0000-4000-8000-000000000001';
DELETE FROM biz_stock_transfer WHERE id = '0f520000-0000-4000-8000-000000000001';

DELETE FROM biz_stock_take_item WHERE take_id = '0f530000-0000-4000-8000-000000000001';
DELETE FROM biz_stock_take WHERE id = '0f530000-0000-4000-8000-000000000001';

DELETE FROM biz_purchase_return_item WHERE return_id = '0f540000-0000-4000-8000-000000000010';
DELETE FROM biz_purchase_return WHERE id = '0f540000-0000-4000-8000-000000000010';
DELETE FROM biz_stock_in_item WHERE stock_in_id = '0f540000-0000-4000-8000-000000000008';
DELETE FROM biz_stock_in WHERE id = '0f540000-0000-4000-8000-000000000008';
DELETE FROM biz_purchase_order WHERE id = '0f540000-0000-4000-8000-00000000000a';

DELETE FROM biz_sales_return_item WHERE return_id = '0f550000-0000-4000-8000-000000000010';
DELETE FROM biz_sales_return WHERE id = '0f550000-0000-4000-8000-000000000010';
DELETE FROM biz_shipment WHERE id = '0f550000-0000-4000-8000-000000000003';
DELETE FROM biz_sales_order WHERE id = '0f550000-0000-4000-8000-000000000002';

DELETE FROM biz_material_serial WHERE material_id IN (
  '0f510000-0000-4000-8000-000000000011', '0f510000-0000-4000-8000-000000000012',
  '0f510000-0000-4000-8000-000000000013', '0f510000-0000-4000-8000-000000000014');
DELETE FROM biz_inventory_location WHERE id IN (
  '0f510000-0000-4000-8000-000000000061', '0f510000-0000-4000-8000-000000000062',
  '0f510000-0000-4000-8000-000000000063', '0f510000-0000-4000-8000-000000000064',
  '0f510000-0000-4000-8000-000000000065');
DELETE FROM biz_location WHERE id IN (
  '0f510000-0000-4000-8000-000000000051', '0f510000-0000-4000-8000-000000000052');
DELETE FROM biz_material WHERE id IN (
  '0f510000-0000-4000-8000-000000000011', '0f510000-0000-4000-8000-000000000012',
  '0f510000-0000-4000-8000-000000000013', '0f510000-0000-4000-8000-000000000014');
DELETE FROM biz_supplier WHERE id = '0f510000-0000-4000-8000-000000000021';
DELETE FROM biz_customer WHERE id = '0f510000-0000-4000-8000-000000000022';
DELETE FROM biz_warehouse WHERE id IN (
  '0f510000-0000-4000-8000-000000000031', '0f510000-0000-4000-8000-000000000032',
  '0f510000-0000-4000-8000-000000000033');
DELETE FROM sys_user WHERE id = '0f510000-0000-4000-8000-000000000041';
DELETE FROM biz_unit WHERE id IN (
  '0f510000-0000-4000-8000-000000000001', '0f510000-0000-4000-8000-000000000002');

-- ============================ 主数据 ============================
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order) VALUES
 ('0f510000-0000-4000-8000-000000000001', 'PDF04-TAI', '台', 1, 1, 94),
 ('0f510000-0000-4000-8000-000000000002', 'PDF04-JIAN', '件', 1, 1, 93);

INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                          safety_stock, costing_method, tax_rate, status, serial_required) VALUES
 ('0f510000-0000-4000-8000-000000000011', 'PDF04-MAT-001', '不锈钢球阀', 'DN50/PN16',
  '0f510000-0000-4000-8000-000000000001', 'FINISHED', 0.0000, 'MOVING_AVG', 0.1300, 1, TRUE),
 ('0f510000-0000-4000-8000-000000000012', 'PDF04-MAT-002', '不锈钢法兰', 'DN80/PN16',
  '0f510000-0000-4000-8000-000000000002', 'RAW', 0.0000, 'MOVING_AVG', 0.1300, 1, FALSE),
 ('0f510000-0000-4000-8000-000000000013', 'PDF04-MAT-003', '碳钢弯头', 'DN25/PN16',
  '0f510000-0000-4000-8000-000000000002', 'RAW', 0.0000, 'MOVING_AVG', 0.1300, 1, FALSE),
 ('0f510000-0000-4000-8000-000000000014', 'PDF04-MAT-004', '止回阀', 'DN100/PN16',
  '0f510000-0000-4000-8000-000000000001', 'FINISHED', 0.0000, 'MOVING_AVG', 0.1300, 1, FALSE);

INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
VALUES ('0f510000-0000-4000-8000-000000000021', 'PDF04-SUP', 'PDF04 阀门制造有限公司', 1);

INSERT INTO biz_customer (id, code, name, credit_days, credit_limit, credit_used, status)
VALUES ('0f510000-0000-4000-8000-000000000022', 'PDF04-CUS', 'PDF04 华东贸易有限公司', 30, 0, 0, 'ACTIVE');

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status) VALUES
 ('0f510000-0000-4000-8000-000000000031', 'WH-PDF04-A', 'PDF04 上海一号仓',
  '/0f510000-0000-4000-8000-000000000031/', 1, 1, 1),
 ('0f510000-0000-4000-8000-000000000032', 'WH-PDF04-B', 'PDF04 上海二号仓',
  '/0f510000-0000-4000-8000-000000000032/', 1, 1, 1),
 ('0f510000-0000-4000-8000-000000000033', 'WH-PDF04-T', 'PDF04 在途仓',
  '/0f510000-0000-4000-8000-000000000033/', 1, 1, 1);

INSERT INTO sys_user (id, username, password, real_name, status)
VALUES ('0f510000-0000-4000-8000-000000000041', 'pdf04.op', 'x', '调拨盘点测试员', 1);

INSERT INTO biz_location (id, location_code, location_name, warehouse_id, status) VALUES
 ('0f510000-0000-4000-8000-000000000051', 'A-01-03', 'A 区 01 排 03 位',
  '0f510000-0000-4000-8000-000000000031', 1),
 ('0f510000-0000-4000-8000-000000000052', 'B-02-04', 'B 区 02 排 04 位',
  '0f510000-0000-4000-8000-000000000032', 1);

INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty) VALUES
 ('0f510000-0000-4000-8000-000000000061', '0f510000-0000-4000-8000-000000000011',
  '0f510000-0000-4000-8000-000000000031', '0f510000-0000-4000-8000-000000000051', 0),
 ('0f510000-0000-4000-8000-000000000062', '0f510000-0000-4000-8000-000000000011',
  '0f510000-0000-4000-8000-000000000032', '0f510000-0000-4000-8000-000000000052', 0),
 ('0f510000-0000-4000-8000-000000000063', '0f510000-0000-4000-8000-000000000012',
  '0f510000-0000-4000-8000-000000000031', NULL, 0),
 ('0f510000-0000-4000-8000-000000000064', '0f510000-0000-4000-8000-000000000013',
  '0f510000-0000-4000-8000-000000000032', NULL, 0),
 ('0f510000-0000-4000-8000-000000000065', '0f510000-0000-4000-8000-000000000014',
  '0f510000-0000-4000-8000-000000000031', NULL, 0);

-- ============================ ① 调拨单（DONE，4 行） ============================
INSERT INTO biz_stock_transfer
  (id, transfer_no, from_warehouse_id, to_warehouse_id, transit_warehouse_id, status,
   transfer_date, reason, applicant_id, applicant_name, approver_id,
   total_qty, total_amount, submitted_at, approved_at, shipped_at, received_at,
   remark, create_by)
VALUES
  ('0f520000-0000-4000-8000-000000000001', 'PDF04-TK-000001',
   '0f510000-0000-4000-8000-000000000031', '0f510000-0000-4000-8000-000000000032',
   '0f510000-0000-4000-8000-000000000033', 'DONE', '2026-11-08', '二号仓备货，平衡区域库存',
   '0f510000-0000-4000-8000-000000000041', '调拨盘点测试员', NULL,
   37.5, 555.60, now(), now(), now(), now(), '两仓已实物清点', '0f510000-0000-4000-8000-000000000041');

INSERT INTO biz_stock_transfer_item
  (id, transfer_id, line_no, material_id, from_location_id, to_location_id,
   qty, shipped_qty, received_qty, unit_cost, amount, remark) VALUES
 ('0f520000-0000-4000-8000-000000000011', '0f520000-0000-4000-8000-000000000001', 1,
  '0f510000-0000-4000-8000-000000000011', '0f510000-0000-4000-8000-000000000061',
  '0f510000-0000-4000-8000-000000000062', 10, 10, 10, 12.340000, 123.40, NULL),
 ('0f520000-0000-4000-8000-000000000012', '0f520000-0000-4000-8000-000000000001', 2,
  '0f510000-0000-4000-8000-000000000012', '0f510000-0000-4000-8000-000000000063',
  NULL, 20, 20, 20, 8.560000, 171.20, NULL),
 ('0f520000-0000-4000-8000-000000000013', '0f520000-0000-4000-8000-000000000001', 3,
  '0f510000-0000-4000-8000-000000000013', NULL,
  '0f510000-0000-4000-8000-000000000064', 5, 5, 5, 5.200000, 26.00, '小件备件'),
 ('0f520000-0000-4000-8000-000000000014', '0f520000-0000-4000-8000-000000000001', 4,
  '0f510000-0000-4000-8000-000000000014', '0f510000-0000-4000-8000-000000000065',
  NULL, 2.5, 2.5, 2.5, 94.000000, 235.00, NULL);

-- ============================ ② 盘点单（PENDING_POST，60 行 → 跨页） ============================
INSERT INTO biz_stock_take
  (id, take_no, warehouse_id, take_date, take_type, status, remark, create_by)
VALUES
  ('0f530000-0000-4000-8000-000000000001', 'PDF04-ST-000001',
   '0f510000-0000-4000-8000-000000000031', '2026-11-08', 'FULL', 'PENDING_POST',
   '年末全盘：范围 = 一号仓全部库位', '0f510000-0000-4000-8000-000000000041');

-- 60 行：物料在 4 个主档间轮换，库位只有前 4 行落真实桶（其余「未分配」桶）
INSERT INTO biz_stock_take_item
  (id, take_id, line_no, material_id, unit_id, location_id, book_qty, actual_qty,
   unit_cost, diff_amount, remark)
SELECT
  ('0f530000-0000-4000-8000-' || lpad((100 + g)::text, 12, '0'))::uuid,
  '0f530000-0000-4000-8000-000000000001'::uuid,
  g,
  (ARRAY['0f510000-0000-4000-8000-000000000011',
         '0f510000-0000-4000-8000-000000000012',
         '0f510000-0000-4000-8000-000000000013',
         '0f510000-0000-4000-8000-000000000014'])[((g - 1) % 4) + 1]::uuid,
  CASE WHEN ((g - 1) % 4) IN (0, 3)
       THEN '0f510000-0000-4000-8000-000000000001'::uuid
       ELSE '0f510000-0000-4000-8000-000000000002'::uuid END,
  CASE g WHEN 1 THEN '0f510000-0000-4000-8000-000000000061'::uuid
         WHEN 2 THEN '0f510000-0000-4000-8000-000000000063'::uuid
         WHEN 3 THEN '0f510000-0000-4000-8000-000000000065'::uuid
         WHEN 4 THEN '0f510000-0000-4000-8000-000000000064'::uuid
         ELSE NULL END,
  (g * 7)::numeric(18,4),
  -- 第 5、17、33 行盘亏 3 / 盘盈 2 / 盘亏 1，其余账实相符
  CASE g WHEN 5 THEN (g * 7 - 3)::numeric(18,4)
         WHEN 17 THEN (g * 7 + 2)::numeric(18,4)
         WHEN 33 THEN (g * 7 - 1)::numeric(18,4)
         ELSE (g * 7)::numeric(18,4) END,
  12.340000,
  0,
  CASE g WHEN 5 THEN '实物短缺 3 台' WHEN 17 THEN '多出 2 台（上批漏记）'
         WHEN 33 THEN '实物短缺 1 台' ELSE NULL END
FROM generate_series(1, 60) AS g;

UPDATE biz_stock_take_item
   SET diff_amount = round(abs(diff_qty) * unit_cost, 4)
 WHERE take_id = '0f530000-0000-4000-8000-000000000001';

UPDATE biz_stock_take t SET
  total_book_qty   = s.book,
  total_actual_qty = s.actual,
  total_gain_qty   = s.gain_qty,
  total_loss_qty   = s.loss_qty,
  total_gain_amount = s.gain_amt,
  total_loss_amount = s.loss_amt
FROM (
  SELECT sum(book_qty)::numeric(18,4) AS book,
         sum(actual_qty)::numeric(18,4) AS actual,
         sum(greatest(diff_qty, 0))::numeric(18,4) AS gain_qty,
         sum(greatest(-diff_qty, 0))::numeric(18,4) AS loss_qty,
         sum(CASE WHEN diff_qty > 0 THEN diff_amount ELSE 0 END)::numeric(18,4) AS gain_amt,
         sum(CASE WHEN diff_qty < 0 THEN diff_amount ELSE 0 END)::numeric(18,4) AS loss_amt
    FROM biz_stock_take_item
   WHERE take_id = '0f530000-0000-4000-8000-000000000001'
) s
WHERE t.id = '0f530000-0000-4000-8000-000000000001';

-- ============================ ③ 采购退货单（DRAFT，2 行） ============================
INSERT INTO biz_purchase_order
  (id, order_no, supplier_id, order_date, expect_date, warehouse_id, status,
   currency_code, total_qty, total_amount, total_tax, total_with_tax, credit_days,
   supplier_name_snapshot, warehouse_name_snapshot, create_by)
VALUES
  ('0f540000-0000-4000-8000-00000000000a', 'PDF04-PO-000001',
   '0f510000-0000-4000-8000-000000000021', '2026-11-01', '2026-11-05',
   '0f510000-0000-4000-8000-000000000031', 'APPROVED', 'CNY', 60, 1946.90, 253.10, 2200.00, 30,
   'PDF04 阀门制造有限公司', 'PDF04 上海一号仓', '0f510000-0000-4000-8000-000000000041');

INSERT INTO biz_stock_in
  (id, in_no, in_type, source_order_id, is_sourceless, supplier_id, warehouse_id,
   in_date, status, currency_code, total_qty, total_amount, total_tax, total_with_tax,
   remark, create_by)
VALUES
  ('0f540000-0000-4000-8000-000000000008', 'PDF04-IN-000001', 'PURCHASE',
   '0f540000-0000-4000-8000-00000000000a'::uuid, FALSE,
   '0f510000-0000-4000-8000-000000000021', '0f510000-0000-4000-8000-000000000031',
   '2026-11-05', 'POSTED', 'CNY', 60, 1946.90, 253.10, 2200.00,
   '外包装完好，随货附材质证明书', '0f510000-0000-4000-8000-000000000041');

INSERT INTO biz_stock_in_item
  (id, stock_in_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate,
   amount, tax_amount, with_tax_amount, location_id, lot_no) VALUES
 ('0f540000-0000-4000-8000-000000000021', '0f540000-0000-4000-8000-000000000008', 1,
  '0f510000-0000-4000-8000-000000000011', '0f510000-0000-4000-8000-000000000001',
  40, 12.34, 13.9442, 13, 493.60, 64.17, 557.77,
  '0f510000-0000-4000-8000-000000000061', 'LOT-PDF04-A'),
 ('0f540000-0000-4000-8000-000000000022', '0f540000-0000-4000-8000-000000000008', 2,
  '0f510000-0000-4000-8000-000000000012', '0f510000-0000-4000-8000-000000000002',
  20, 72.665, 82.1115, 13, 1453.30, 188.93, 1642.23,
  '0f510000-0000-4000-8000-000000000063', 'LOT-PDF04-B');

INSERT INTO biz_purchase_return
  (id, return_no, stock_in_id, purchase_order_id, supplier_id, warehouse_id, return_date,
   status, total_qty, total_amount, total_tax, total_with_tax, reason, remark, create_by)
VALUES
  ('0f540000-0000-4000-8000-000000000010', 'PDF04-PRT-000001',
   '0f540000-0000-4000-8000-000000000008'::uuid,
   '0f540000-0000-4000-8000-00000000000a'::uuid,
   '0f510000-0000-4000-8000-000000000021', '0f510000-0000-4000-8000-000000000031',
   '2026-11-08', 'DRAFT', 6, 86.46, 11.24, 97.70,
   '阀体密封面有砂眼，供应商书面同意退货，运费由供方承担',
   '随退货附质检报告一份（编号 QC-PDF04-0001）', '0f510000-0000-4000-8000-000000000041');

INSERT INTO biz_purchase_return_item
  (id, return_id, line_no, stock_in_item_id, purchase_order_item_id, material_id, unit_id,
   qty, price, price_incl, tax_rate, amount, tax_amount, with_tax_amount, location_id, remark) VALUES
 ('0f540000-0000-4000-8000-000000000031', '0f540000-0000-4000-8000-000000000010', 1,
  '0f540000-0000-4000-8000-000000000021', NULL,
  '0f510000-0000-4000-8000-000000000011', '0f510000-0000-4000-8000-000000000001',
  4, 12.34, 13.9442, 13, 49.36, 6.42, 55.78,
  '0f510000-0000-4000-8000-000000000061', '密封面砂眼'),
 ('0f540000-0000-4000-8000-000000000032', '0f540000-0000-4000-8000-000000000010', 2,
  '0f540000-0000-4000-8000-000000000022', NULL,
  '0f510000-0000-4000-8000-000000000012', '0f510000-0000-4000-8000-000000000002',
  2, 18.55, 20.9615, 13, 37.10, 4.82, 41.92,
  '0f510000-0000-4000-8000-000000000063', '尺寸超差');

-- ============================ ④ 销售退货单（DRAFT，2 行，带序列号） ============================
INSERT INTO biz_sales_order
  (id, so_no, customer_id, order_date, delivery_date, status,
   total_amount, total_tax, total_with_tax, remark, create_by)
VALUES
  ('0f550000-0000-4000-8000-000000000002', 'PDF04-SO-000001',
   '0f510000-0000-4000-8000-000000000022', '2026-11-01', '2026-11-06',
   'APPROVED', 500.00, 65.00, 565.00, 'PDF-04 销售退货来源夹具',
   '0f510000-0000-4000-8000-000000000041');

INSERT INTO biz_shipment
  (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty)
VALUES
  ('0f550000-0000-4000-8000-000000000003', 'PDF04-SH-000001',
   '0f550000-0000-4000-8000-000000000002'::uuid,
   '0f510000-0000-4000-8000-000000000022'::uuid,
   '0f510000-0000-4000-8000-000000000031'::uuid, '2026-11-06', 'POSTED', 10);

INSERT INTO biz_sales_return
  (id, return_no, customer_id, so_id, shipment_id, return_date, return_type, warehouse_id,
   status, total_amount, total_tax, total_with_tax, remark, create_by)
VALUES
  ('0f550000-0000-4000-8000-000000000010', 'PDF04-SR-000001',
   '0f510000-0000-4000-8000-000000000022'::uuid,
   '0f550000-0000-4000-8000-000000000002'::uuid,
   '0f550000-0000-4000-8000-000000000003'::uuid, '2026-11-08', 'QUALITY',
   '0f510000-0000-4000-8000-000000000032'::uuid, 'DRAFT',
   200.00, 26.00, 226.00, '客户反馈焊缝渗漏，整批退回并已拍照留证',
   '0f510000-0000-4000-8000-000000000041');

INSERT INTO biz_sales_return_item
  (id, return_id, so_item_id, material_id, qty, price, amount, tax_rate, tax_amount,
   location_id, serial_nos, remark) VALUES
 ('0f550000-0000-4000-8000-000000000031', '0f550000-0000-4000-8000-000000000010',
  NULL, '0f510000-0000-4000-8000-000000000011', 2, 12.34, 24.68, 13, 3.21,
  '0f510000-0000-4000-8000-000000000052', '{PDF04-SN-000001,PDF04-SN-000002}', '焊缝渗漏'),
 ('0f550000-0000-4000-8000-000000000032', '0f550000-0000-4000-8000-000000000010',
  NULL, '0f510000-0000-4000-8000-000000000014', 1, 175.32, 175.32, 13, 22.79,
  NULL, '{PDF04-SN-000003}', '整仓退回（未指定库位）');

COMMIT;

-- 自检：四张单据 + 明细行数
SELECT 'transfer' AS doc, transfer_no AS no, total_qty, total_amount FROM biz_stock_transfer
 WHERE id = '0f520000-0000-4000-8000-000000000001'
UNION ALL SELECT 'stock_take', take_no, total_book_qty, total_gain_amount + total_loss_amount
 FROM biz_stock_take WHERE id = '0f530000-0000-4000-8000-000000000001'
UNION ALL SELECT 'purchase_return', return_no, total_qty, total_with_tax
 FROM biz_purchase_return WHERE id = '0f540000-0000-4000-8000-000000000010'
UNION ALL SELECT 'sales_return', return_no, 0, total_with_tax
 FROM biz_sales_return WHERE id = '0f550000-0000-4000-8000-000000000010';
