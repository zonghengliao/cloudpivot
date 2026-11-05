-- 形态④ 夹具：一条「被豁免的流水」指向一条已软删的序列号
--   行 id = 生产豁免台账 di.v_poly_ref_waiver 里登记的那一行（01a19108-ee53-…4ea963）
--   有效期内 ⇒ DI-18 [DEFECT/INFO] 快照带「已核查豁免#」；expires_on 改成昨天 ⇒ 回 ERROR
BEGIN;
INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status, deleted_at)
VALUES ('01a1f900-0000-7000-8000-0000000000d1', 'SN-D4-DI18-000001',
        'e2040000-0000-4000-8000-000000000011', 'e2040000-0000-4000-8000-000000000003',
        'IN_STOCK', now());

INSERT INTO biz_inventory_txn
  (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
   qty, unit_cost, amount, before_qty, before_amount, before_cost,
   after_qty, after_amount, after_cost, period_code, operator_id, remark, created_at)
VALUES
  ('01a19108-ee53-73c6-bdaa-0109614ea963',
   'e2040000-0000-4000-8000-000000000011', 'e2040000-0000-4000-8000-000000000003',
   'IN', 'PURCHASE_IN', 'SERIAL_BATCH', '01a1f900-0000-7000-8000-0000000000d1',
   'D4-DI18-WAIVER-0001', 1, 10, 0, 0, 0, 0, 0, 10, 0, 0, '2026-11', NULL,
   '收口D 验收夹具：DI-18 已核查豁免/到期回红（非生产数据）', now());

-- 形态①b 反证：同一 doc_type=SERIAL_BATCH、同一目标表，但行 id **不在**豁免台账
INSERT INTO biz_inventory_txn
  (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
   qty, unit_cost, amount, before_qty, before_amount, before_cost,
   after_qty, after_amount, after_cost, period_code, operator_id, remark, created_at)
VALUES
  ('01a1f900-0000-7000-8000-0000000000e2',
   'e2040000-0000-4000-8000-000000000011', 'e2040000-0000-4000-8000-000000000003',
   'IN', 'PURCHASE_IN', 'SERIAL_BATCH', '01a1f900-0000-7000-8000-0000000000d1',
   'D4-DI18-NOWAIVER', 1, 5, 0, 0, 0, 0, 0, 5, 0, 0, '2026-11', NULL,
   '收口D 验收反证：同 doc_type 同目标表，行 id 未登记 ⇒ 必须仍 ERROR', now());
COMMIT;
