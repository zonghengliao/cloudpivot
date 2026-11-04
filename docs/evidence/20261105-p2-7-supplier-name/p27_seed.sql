-- 与 p27_fixture.sql 同一批变异，但**提交**（只灌进一次性证据库 cloudpivot_54ad4c5f_seeded）
\set ON_ERROR_STOP on
BEGIN;

INSERT INTO biz_supplier (supplier_code, supplier_name, short_name, status, credit_days, credit_limit)
VALUES ('P27-DUP-A', 'P2-7 同名测试供应商', NULL, 1, 0, 0),
       ('P27-DUP-B', 'p2-7 同名测试供应商  ', NULL, 1, 0, 0);

INSERT INTO biz_supplier (supplier_code, supplier_name, short_name, status, credit_days, credit_limit)
VALUES ('P27-SHT-A', 'P2-7 简称甲（全名不同）', '同一简称', 1, 0, 0),
       ('P27-SHT-B', 'P2-7 简称乙（全名不同）', '  同一简称 ', 1, 0, 0);

INSERT INTO biz_supplier (supplier_code, supplier_name, short_name, status, credit_days, credit_limit)
VALUES ('P27-SHW-A', 'P2-7 撞名测试供应商', 'P7 付款测试供应商', 1, 0, 0);

UPDATE biz_purchase_order
   SET supplier_name_snapshot = 'W1B 旧名（改名前的名字）', created_at = now()
 WHERE order_no = 'W1B-PO-0001';

UPDATE biz_contract
   SET party_b_name = party_b_name || '（旧名）'
 WHERE id = (SELECT id FROM biz_contract
              WHERE deleted_at IS NULL AND party_b_id IS NOT NULL
              ORDER BY contract_no LIMIT 1);

UPDATE biz_flow_instance
   SET form_data = jsonb_set(form_data, '{supplier_name}',
                             to_jsonb('P17 付款审批测试供应商（旧名）'::text))
 WHERE biz_no = 'PM-20261105-000048';

COMMIT;
