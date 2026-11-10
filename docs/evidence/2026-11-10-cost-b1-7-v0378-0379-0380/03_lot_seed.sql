-- B1-7 批号回填演练种子数据（V0380 用）
INSERT INTO biz_stock_in (id, in_no, warehouse_id, in_date, in_type, is_sourceless)
VALUES ('20000000-0000-4000-8000-0000000b1710'::uuid, 'B17-IN-001', gen_random_uuid(), DATE '2026-10-01', 'OTHER', TRUE);
INSERT INTO biz_stock_out (id, out_no, warehouse_id, out_date, out_type)
VALUES ('20000000-0000-4000-8000-0000000b1720'::uuid, 'B17-OUT-001', gen_random_uuid(), DATE '2026-10-02', 'ISSUE');

INSERT INTO biz_material (id, material_code, material_name, unit_id)
VALUES ('20000000-0000-4000-8000-0000000b1701'::uuid, 'B17-M1', 'B1-7 演练物料1', gen_random_uuid()),
       ('20000000-0000-4000-8000-0000000b1702'::uuid, 'B17-M2', 'B1-7 演练物料2', gen_random_uuid()),
       ('20000000-0000-4000-8000-0000000b1703'::uuid, 'B17-M3', 'B1-7 演练物料3(已软删)', gen_random_uuid());
UPDATE biz_material SET deleted_at = CURRENT_TIMESTAMP WHERE id = '20000000-0000-4000-8000-0000000b1703'::uuid;

-- 入库明细：M1 批号 ' B17-LOT-A '（含首尾空格）、M1 批号 'B17-LOT-B'、M3（软删物料）批号 'B17-LOT-C'
INSERT INTO biz_stock_in_item (stock_in_id, line_no, material_id, unit_id, qty, lot_no)
VALUES ('20000000-0000-4000-8000-0000000b1710'::uuid, 1, '20000000-0000-4000-8000-0000000b1701'::uuid, gen_random_uuid(), 10, ' B17-LOT-A '),
       ('20000000-0000-4000-8000-0000000b1710'::uuid, 2, '20000000-0000-4000-8000-0000000b1701'::uuid, gen_random_uuid(), 5,  'B17-LOT-B'),
       ('20000000-0000-4000-8000-0000000b1710'::uuid, 3, '20000000-0000-4000-8000-0000000b1703'::uuid, gen_random_uuid(), 7,  'B17-LOT-C');

-- 出库明细：M1 批号 'B17-LOT-A'（与入库同一批号，应合并成一行，remark 来源取 STOCK_IN）、M2 批号 'B17-LOT-D'
INSERT INTO biz_stock_out_item (stock_out_id, line_no, material_id, unit_id, qty, lot_no)
VALUES ('20000000-0000-4000-8000-0000000b1720'::uuid, 1, '20000000-0000-4000-8000-0000000b1701'::uuid, gen_random_uuid(), 3, 'B17-LOT-A'),
       ('20000000-0000-4000-8000-0000000b1720'::uuid, 2, '20000000-0000-4000-8000-0000000b1702'::uuid, gen_random_uuid(), 2, 'B17-LOT-D');

-- 序列号：M2 批号 'B17-LOT-E'（只有这条来源）；已软删的序列号不参与
INSERT INTO biz_material_serial (serial_no, material_id, status, batch_no)
VALUES ('B17-SN-001', '20000000-0000-4000-8000-0000000b1702'::uuid, 'IN_STOCK', 'B17-LOT-E'),
       ('B17-SN-002', '20000000-0000-4000-8000-0000000b1702'::uuid, 'IN_STOCK', 'B17-LOT-GONE');
UPDATE biz_material_serial SET deleted_at = CURRENT_TIMESTAMP WHERE serial_no = 'B17-SN-002';

-- 客户已维护过的批次（同 M1 / B17-LOT-B，source_type=PRODUCTION）→ 回填不得覆盖
INSERT INTO biz_lot (lot_no, material_id, source_type, source_doc_type, remark)
VALUES ('B17-LOT-B', '20000000-0000-4000-8000-0000000b1701'::uuid, 'PRODUCTION', 'COST_PERIOD', '客户自建生产批，不许覆盖');
