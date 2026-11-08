-- PDF-03 @145 隔离实例夹具（库 cloudpivot_pdf03，命名空间 0f05/0f06）
-- 目的：给三张单据（合同 / 收货入库单 / 出库单）造出真实数据，供 curl 实测用。
-- 全部用固定 UUID，重复执行先删后插（幂等）。
\set ON_ERROR_STOP on

BEGIN;

-- ---------- 清场 ----------
DELETE FROM biz_material_serial WHERE source_id IN
  ('0f050000-0000-4000-8000-000000000010', '0f050000-0000-4000-8000-000000000020');
DELETE FROM biz_stock_in_item WHERE stock_in_id = '0f050000-0000-4000-8000-000000000010';
DELETE FROM biz_stock_in WHERE id = '0f050000-0000-4000-8000-000000000010';
DELETE FROM biz_stock_out_item WHERE stock_out_id = '0f050000-0000-4000-8000-000000000020';
DELETE FROM biz_stock_out WHERE id = '0f050000-0000-4000-8000-000000000020';
DELETE FROM biz_shipment WHERE id = '0f050000-0000-4000-8000-000000000030';
DELETE FROM biz_sales_order WHERE id = '0f050000-0000-4000-8000-000000000031';
DELETE FROM biz_purchase_order WHERE id = '0f050000-0000-4000-8000-000000000040';
DELETE FROM biz_doc_attachment WHERE doc_id = '0f050000-0000-4000-8000-000000000050';
DELETE FROM sys_file WHERE id IN
  ('0f050000-0000-4000-8000-000000000054',
   '0f050000-0000-4000-8000-000000000055',
   '0f050000-0000-4000-8000-000000000056');
DELETE FROM biz_contract WHERE id = '0f050000-0000-4000-8000-000000000050';
DELETE FROM biz_location WHERE id = '0f050000-0000-4000-8000-000000000009';
DELETE FROM biz_inventory_location WHERE id = '0f050000-0000-4000-8000-000000000008';
DELETE FROM biz_material WHERE id IN
  ('0f050000-0000-4000-8000-000000000002', '0f050000-0000-4000-8000-000000000003');
DELETE FROM biz_supplier WHERE id = '0f050000-0000-4000-8000-000000000004';
DELETE FROM biz_warehouse WHERE id = '0f050000-0000-4000-8000-000000000005';
DELETE FROM biz_customer WHERE id = '0f050000-0000-4000-8000-000000000006';
DELETE FROM sys_user WHERE id = '0f050000-0000-4000-8000-000000000007';
DELETE FROM biz_unit WHERE id = '0f050000-0000-4000-8000-000000000001';

-- ---------- 主数据 ----------
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order)
VALUES ('0f050000-0000-4000-8000-000000000001', 'PDF03-PCS', '台', 1, 1, 96);

INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                          safety_stock, costing_method, tax_rate, status, serial_required)
VALUES
 ('0f050000-0000-4000-8000-000000000002', 'PDF03-MAT-001', '不锈钢球阀', 'DN50/PN16',
  '0f050000-0000-4000-8000-000000000001', 'FINISHED', 0.0000, 'MOVING_AVG', 0.1300, 1, TRUE),
 ('0f050000-0000-4000-8000-000000000003', 'PDF03-MAT-002', '不锈钢法兰', 'DN80/PN16',
  '0f050000-0000-4000-8000-000000000001', 'RAW', 0.0000, 'MOVING_AVG', 0.1300, 1, FALSE);

INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
VALUES ('0f050000-0000-4000-8000-000000000004', 'PDF03-SUP', 'PDF03 测试供应商', 1);

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
VALUES ('0f050000-0000-4000-8000-000000000005', 'WH-PDF03-1', 'PDF03 一号仓',
        '/0f050000-0000-4000-8000-000000000005/', 1, 1, 1);

INSERT INTO biz_customer (id, code, name, credit_days, credit_limit, credit_used, status)
VALUES ('0f050000-0000-4000-8000-000000000006', 'PDF03-CUS', 'PDF03 测试客户', 30, 0, 0, 'ACTIVE');

INSERT INTO sys_user (id, username, password, real_name, status)
VALUES ('0f050000-0000-4000-8000-000000000007', 'pdf03.op', 'x', '出入库测试员', 1);

INSERT INTO biz_location (id, location_code, location_name, warehouse_id, status)
VALUES ('0f050000-0000-4000-8000-000000000009', 'A-01-03', 'A 区 01 排 03 位',
        '0f050000-0000-4000-8000-000000000005', 1);

INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
VALUES ('0f050000-0000-4000-8000-000000000008', '0f050000-0000-4000-8000-000000000002',
        '0f050000-0000-4000-8000-000000000005', '0f050000-0000-4000-8000-000000000009', 0);

-- ---------- 合同（含 3 个附件 + 60 条条款正文 + 付款方式） ----------
INSERT INTO biz_contract
 (id, contract_no, contract_name, contract_type, party_a_name, party_b_type, party_b_id,
  party_b_name, amount, currency_code, sign_date, start_date, end_date, status, owner_id,
  remark, create_by)
VALUES
 ('0f050000-0000-4000-8000-000000000050', 'PDF03-HT-000001', 'PDF03 不锈钢管件年度采购合同',
  'PURCHASE', '云枢（上海）科技有限公司', 'SUPPLIER', '0f050000-0000-4000-8000-000000000004',
  'PDF03 测试供应商', 123456.7800, 'CNY', '2026-11-01', '2026-11-01', '2027-10-31', 'SIGNED',
  '0f050000-0000-4000-8000-000000000007',
  (SELECT string_agg('第' || g || '条 本合同条款示例：甲方应按约定履行第' || g || '项义务，乙方应按时交付并保证质量。', E'\n')
     FROM generate_series(1, 60) g)
  || E'\n付款方式：货到验收后 30 日内电汇付清（质保金 5% 于质保期满后支付）。',
  '0f050000-0000-4000-8000-000000000007');

INSERT INTO sys_file (id, file_name, original_name, file_key, file_size, mime_type)
VALUES
 ('0f050000-0000-4000-8000-000000000054', 'pdf03-contract-1.pdf', 'PDF03-合同扫描件-第1页.pdf',
  'pdf03/contract-1', 268800, 'application/pdf'),
 ('0f050000-0000-4000-8000-000000000055', 'pdf03-other.pdf', 'PDF03-用印授权书.pdf',
  'pdf03/other', 102400, 'application/pdf');

INSERT INTO biz_doc_attachment (id, doc_type, doc_id, file_id, file_role, uploaded_by)
VALUES
 ('0f050000-0000-4000-8000-000000000051', 'CONTRACT', '0f050000-0000-4000-8000-000000000050',
  '0f050000-0000-4000-8000-000000000054', 'CONTRACT_SCAN', '0f050000-0000-4000-8000-000000000007'),
 ('0f050000-0000-4000-8000-000000000052', 'CONTRACT', '0f050000-0000-4000-8000-000000000050',
  '0f050000-0000-4000-8000-000000000055', 'SUPPLEMENT', '0f050000-0000-4000-8000-000000000007');

-- ---------- 采购订单（入库单的来源单据） ----------
INSERT INTO biz_purchase_order
 (id, order_no, supplier_id, order_date, expect_date, warehouse_id, status, currency_code,
  total_qty, total_amount, total_tax, total_with_tax, credit_days, remark, create_by,
  supplier_name_snapshot, warehouse_name_snapshot)
VALUES
 ('0f050000-0000-4000-8000-000000000040', 'PDF03-PO-000001', '0f050000-0000-4000-8000-000000000004',
  '2026-11-01', '2026-11-08', '0f050000-0000-4000-8000-000000000005', 'APPROVED', 'CNY',
  12.5, 373.45, 48.55, 422.00, 30, 'PDF-03 入库来源夹具',
  '0f050000-0000-4000-8000-000000000007', 'PDF03 测试供应商', 'PDF03 一号仓');

-- ---------- 收货入库单（3 行，第一行带批次号 + 库位 + 3 个序列号） ----------
INSERT INTO biz_stock_in
 (id, in_no, in_type, source_order_id, is_sourceless, supplier_id, warehouse_id, in_date,
  status, currency_code, total_qty, total_amount, total_tax, total_with_tax, remark, create_by)
VALUES
 ('0f050000-0000-4000-8000-000000000010', 'PDF03-IN-000001', 'PURCHASE',
  '0f050000-0000-4000-8000-000000000040', FALSE, '0f050000-0000-4000-8000-000000000004',
  '0f050000-0000-4000-8000-000000000005', '2026-11-08', 'POSTED', 'CNY',
  13.5, 373.45, 48.55, 422.00, '外包装完好，随货附材质证明书',
  '0f050000-0000-4000-8000-000000000007');

INSERT INTO biz_stock_in_item
 (id, stock_in_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate,
  amount, tax_amount, with_tax_amount, location_id, lot_no, remark)
VALUES
 ('0f050000-0000-4000-8000-000000000011', '0f050000-0000-4000-8000-000000000010', 1,
  '0f050000-0000-4000-8000-000000000002', '0f050000-0000-4000-8000-000000000001',
  10, 12.340000, 13.944200, 13, 123.40, 16.042, 139.442,
  '0f050000-0000-4000-8000-000000000008', 'LOT-20261108-A', '一物一码，逐个扫码登记'),
 ('0f050000-0000-4000-8000-000000000012', '0f050000-0000-4000-8000-000000000010', 2,
  '0f050000-0000-4000-8000-000000000003', '0f050000-0000-4000-8000-000000000001',
  2.5, 100.000000, 113.000000, 13, 250.00, 32.50, 282.50, NULL, 'LOT-20261108-B', '整箱到货'),
 ('0f050000-0000-4000-8000-000000000013', '0f050000-0000-4000-8000-000000000010', 3,
  '0f050000-0000-4000-8000-000000000002', '0f050000-0000-4000-8000-000000000001',
  1, 0.050000, 0.056500, 13, 0.05, 0.0065, 0.0565, NULL, NULL, '备件');

INSERT INTO biz_material_serial
 (id, serial_no, material_id, warehouse_id, location_id, status, source_type, source_id)
VALUES
 (gen_random_uuid(), 'PDF03-SN-000001', '0f050000-0000-4000-8000-000000000002',
  '0f050000-0000-4000-8000-000000000005', '0f050000-0000-4000-8000-000000000008',
  'IN_STOCK', 'PURCHASE', '0f050000-0000-4000-8000-000000000010'),
 (gen_random_uuid(), 'PDF03-SN-000002', '0f050000-0000-4000-8000-000000000002',
  '0f050000-0000-4000-8000-000000000005', '0f050000-0000-4000-8000-000000000008',
  'IN_STOCK', 'PURCHASE', '0f050000-0000-4000-8000-000000000010'),
 (gen_random_uuid(), 'PDF03-SN-000003', '0f050000-0000-4000-8000-000000000002',
  '0f050000-0000-4000-8000-000000000005', '0f050000-0000-4000-8000-000000000008',
  'IN_STOCK', 'PURCHASE', '0f050000-0000-4000-8000-000000000010');

-- ---------- 销售订单 + 发货单 + 出库单（50 行，来源 = 发货单） ----------
INSERT INTO biz_sales_order
 (id, so_no, customer_id, order_date, delivery_date, status, total_amount, total_tax,
  total_with_tax, remark, create_by)
VALUES
 ('0f050000-0000-4000-8000-000000000031', 'PDF03-SO-000001', '0f050000-0000-4000-8000-000000000006',
  '2026-11-01', '2026-11-08', 'APPROVED', 500.00, 65.00, 565.00, 'PDF-03 出库来源夹具',
  '0f050000-0000-4000-8000-000000000007');

INSERT INTO biz_shipment
 (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty, stock_out_id)
VALUES
 ('0f050000-0000-4000-8000-000000000030', 'PDF03-SH-000001',
  '0f050000-0000-4000-8000-000000000031', '0f050000-0000-4000-8000-000000000006',
  '0f050000-0000-4000-8000-000000000005', '2026-11-08', 'POSTED', 50,
  '0f050000-0000-4000-8000-000000000020');

INSERT INTO biz_stock_out
 (id, out_no, out_type, warehouse_id, out_date, status, total_qty, total_amount, reason,
  remark, source_type, source_id, create_by)
VALUES
 ('0f050000-0000-4000-8000-000000000020', 'PDF03-OUT-000001', 'OTHER',
  '0f050000-0000-4000-8000-000000000005', '2026-11-08', 'POSTED', 50, 617.00,
  '销售发货过账自动生成', '客户指定物流，出库前已点数', 'SALES',
  '0f050000-0000-4000-8000-000000000030', '0f050000-0000-4000-8000-000000000007');

INSERT INTO biz_stock_out_item
 (id, stock_out_id, line_no, material_id, unit_id, qty, unit_cost, amount, location_id, lot_no, remark)
SELECT gen_random_uuid(), '0f050000-0000-4000-8000-000000000020', g,
       CASE WHEN g % 2 = 0 THEN '0f050000-0000-4000-8000-000000000002'::uuid
            ELSE '0f050000-0000-4000-8000-000000000003'::uuid END,
       '0f050000-0000-4000-8000-000000000001'::uuid, 1, 12.340000, 12.34,
       '0f050000-0000-4000-8000-000000000008'::uuid, 'LOT-20261108-A', NULL
  FROM generate_series(1, 50) g;

COMMIT;

\echo '--- 夹具就绪 ---'
SELECT 'contract' AS kind, contract_no AS doc_no, amount::text AS amount
  FROM biz_contract WHERE id = '0f050000-0000-4000-8000-000000000050'
UNION ALL
SELECT 'stock_in', in_no, total_with_tax::text FROM biz_stock_in
 WHERE id = '0f050000-0000-4000-8000-000000000010'
UNION ALL
SELECT 'stock_out', out_no, total_amount::text FROM biz_stock_out
 WHERE id = '0f050000-0000-4000-8000-000000000020';
SELECT (SELECT count(*) FROM biz_stock_in_item WHERE stock_in_id='0f050000-0000-4000-8000-000000000010') AS in_rows,
       (SELECT count(*) FROM biz_material_serial WHERE source_id='0f050000-0000-4000-8000-000000000010') AS serials,
       (SELECT count(*) FROM biz_stock_out_item WHERE stock_out_id='0f050000-0000-4000-8000-000000000020') AS out_rows,
       (SELECT count(*) FROM biz_doc_attachment WHERE doc_id='0f050000-0000-4000-8000-000000000050') AS attachments;
