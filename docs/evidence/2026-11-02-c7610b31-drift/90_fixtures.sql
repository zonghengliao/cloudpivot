-- ============================================================
-- 90_fixtures.sql —— 巡检自证夹具（**只用于验证设计稿，不属于交付物**）
--   目的：在 cp_di_fut（V0193 已应用）上构造每条巡检项的「应该命中」样本，
--        证明 13 条 SQL 真的能抓到各自声称的缺陷，而不是空跑 0 行。
--   全部使用固定 UUID 前缀 f1c00000-… 与单号前缀 FIXDI-，删起来干净。
-- ============================================================
BEGIN;

-- 0) 夹具清场（可重复执行）
DELETE FROM biz_payable          WHERE payable_no LIKE 'FIXDI-%';
DELETE FROM biz_stock_in         WHERE in_no      LIKE 'FIXDI-%';
DELETE FROM biz_purchase_order   WHERE order_no   LIKE 'FIXDI-%';
DELETE FROM biz_contract         WHERE contract_no LIKE 'FIXDI-%';
DELETE FROM biz_supplier         WHERE supplier_code LIKE 'FIXDI-%';
DELETE FROM biz_warehouse        WHERE warehouse_code LIKE 'FIXDI-%';
DELETE FROM biz_contract_party_change WHERE reason LIKE 'FIXDI%';

-- 1) 主数据
INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) VALUES
 ('f1c00000-0000-4000-8000-000000000001','FIXDI-B1','夹具供应商乙1',1),
 ('f1c00000-0000-4000-8000-000000000002','FIXDI-B2','夹具供应商乙2',1),
 ('f1c00000-0000-4000-8000-000000000003','FIXDI-BD','夹具供应商已删',1);
UPDATE biz_supplier SET deleted_at = now() WHERE id = 'f1c00000-0000-4000-8000-000000000003';

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path) VALUES
 ('f1c00000-0000-4000-8000-0000000000f1','FIXDI-W1','夹具仓','/f1c00000-0000-4000-8000-0000000000f1/');

-- 2) 合同：CT1 正常 / CT2 已软删 / CT3 有乙方变更台账
INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                          party_b_id, party_b_name, status, sign_date, amount) VALUES
 ('f1c00000-0000-4000-8000-0000000000c1','FIXDI-CT-1','夹具合同一','PURCHASE','SUPPLIER',
  'f1c00000-0000-4000-8000-000000000001','夹具供应商乙1','ACTIVE', current_date, 1000),
 ('f1c00000-0000-4000-8000-0000000000c2','FIXDI-CT-2','夹具合同二','PURCHASE','SUPPLIER',
  'f1c00000-0000-4000-8000-000000000001','夹具供应商乙1','ACTIVE', current_date, 1000),
 ('f1c00000-0000-4000-8000-0000000000c3','FIXDI-CT-3','夹具合同三','PURCHASE','SUPPLIER',
  'f1c00000-0000-4000-8000-000000000002','夹具供应商乙2','ACTIVE', current_date, 1000),
 -- 已终止的合同（给 DI-15 当上游）
 ('f1c00000-0000-4000-8000-0000000000c4','FIXDI-CT-4','夹具合同四（已终止）','PURCHASE','SUPPLIER',
  'f1c00000-0000-4000-8000-000000000001','夹具供应商乙1','TERMINATED', current_date, 1000);
UPDATE biz_contract SET deleted_at = now() WHERE id = 'f1c00000-0000-4000-8000-0000000000c2';

-- CT3 的乙方变更台账：T = 2026-10-01，B1 → B2
INSERT INTO biz_contract_party_change (id, contract_id, old_party_b_id, new_party_b_id, changed_at, reason)
VALUES ('f1c00000-0000-4000-8000-0000000000e1','f1c00000-0000-4000-8000-0000000000c3',
        'f1c00000-0000-4000-8000-000000000001','f1c00000-0000-4000-8000-000000000002',
        TIMESTAMPTZ '2026-10-01 10:00:00+08', 'FIXDI 合同换乙方（有审批）');

-- 3) 采购单
INSERT INTO biz_purchase_order
 (id, order_no, supplier_id, order_date, warehouse_id, status, purchase_type,
  contract_id, contract_no_snapshot, supplier_name_snapshot, created_at) VALUES
 -- 对齐，不应命中任何项
 ('f1c00000-0000-4000-8000-000000000101','FIXDI-P1','f1c00000-0000-4000-8000-000000000001',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','APPROVED','CONTRACT',
  'f1c00000-0000-4000-8000-0000000000c1','FIXDI-CT-1','夹具供应商乙1', now() - interval '10 day'),
 -- 快照 ≠ 合同乙方，且 CT1 无变更台账 → DI-01 ③ P0（合同粒度）
 ('f1c00000-0000-4000-8000-000000000102','FIXDI-P2','f1c00000-0000-4000-8000-000000000002',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','APPROVED','CONTRACT',
  'f1c00000-0000-4000-8000-0000000000c1','FIXDI-CT-1','夹具供应商乙2', now() - interval '9 day'),
 -- 合同已软删 → DI-02 P0（APPROVED）
 ('f1c00000-0000-4000-8000-000000000103','FIXDI-P3','f1c00000-0000-4000-8000-000000000001',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','APPROVED','CONTRACT',
  'f1c00000-0000-4000-8000-0000000000c2','FIXDI-CT-2','夹具供应商乙1', now() - interval '8 day'),
 -- contract_id 悬空 → DI-03 P0
 ('f1c00000-0000-4000-8000-000000000104','FIXDI-P4','f1c00000-0000-4000-8000-000000000001',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','APPROVED','CONTRACT',
  'f1c00000-0000-4000-8000-0000000deade','FIXDI-CT-X','夹具供应商乙1', now() - interval '7 day'),
 -- 供应商主档已软删 → DI-12
 ('f1c00000-0000-4000-8000-000000000105','FIXDI-P5','f1c00000-0000-4000-8000-000000000003',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','APPROVED','SPOT',
  NULL,NULL,NULL, now() - interval '6 day'),
 -- 供应商悬空 → DI-12（ROW_MISSING）
 ('f1c00000-0000-4000-8000-000000000106','FIXDI-P6','f1c00000-0000-4000-8000-0000000feee1',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','APPROVED','SPOT',
  NULL,NULL,NULL, now() - interval '5 day'),
 -- CT3：建单早于变更（2026-09-01 < 2026-10-01）→ DI-01 ① 合法历史快照（INFO，不告警）
 ('f1c00000-0000-4000-8000-000000000107','FIXDI-P7','f1c00000-0000-4000-8000-000000000001',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','CLOSED','CONTRACT',
  'f1c00000-0000-4000-8000-0000000000c3','FIXDI-CT-3','夹具供应商乙1',
  TIMESTAMPTZ '2026-09-01 09:00:00+08'),
 -- CT3：建单晚于变更（2026-10-05 > 2026-10-01）但快照仍是旧乙方 → DI-01 ② P1
 ('f1c00000-0000-4000-8000-000000000108','FIXDI-P8','f1c00000-0000-4000-8000-000000000001',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','DRAFT','CONTRACT',
  'f1c00000-0000-4000-8000-0000000000c3','FIXDI-CT-3','夹具供应商乙1',
  TIMESTAMPTZ '2026-10-05 09:00:00+08'),
 -- 已软删的采购单（给 DI-07 当上游）
 ('f1c00000-0000-4000-8000-000000000109','FIXDI-P9','f1c00000-0000-4000-8000-000000000001',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','APPROVED','SPOT',
  NULL,NULL,NULL, now() - interval '4 day'),
 -- 引用已终止合同 → DI-15 P0（APPROVED 在途）
 ('f1c00000-0000-4000-8000-000000000110','FIXDI-P10','f1c00000-0000-4000-8000-000000000001',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','APPROVED','CONTRACT',
  'f1c00000-0000-4000-8000-0000000000c4','FIXDI-CT-4',NULL, now() - interval '3 day');
UPDATE biz_purchase_order SET deleted_at = now() WHERE id = 'f1c00000-0000-4000-8000-000000000109';

-- 4) 入库单
INSERT INTO biz_stock_in
 (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status) VALUES
 -- 供应商 ≠ 来源采购单供应商，且已过账 → DI-06 P0；同时 → DI-10
 ('f1c00000-0000-4000-8000-000000000201','FIXDI-S1','PURCHASE',
  'f1c00000-0000-4000-8000-000000000101','f1c00000-0000-4000-8000-000000000002',
  'f1c00000-0000-4000-8000-0000000000f1', current_date, 'POSTED'),
 -- 来源采购单已软删，已过账 → DI-07 P0
 ('f1c00000-0000-4000-8000-000000000202','FIXDI-S2','PURCHASE',
  'f1c00000-0000-4000-8000-000000000109','f1c00000-0000-4000-8000-000000000001',
  'f1c00000-0000-4000-8000-0000000000f1', current_date, 'POSTED'),
 -- 来源采购单悬空 → DI-08 P0
 ('f1c00000-0000-4000-8000-000000000203','FIXDI-S3','PURCHASE',
  'f1c00000-0000-4000-8000-0000000abcd1','f1c00000-0000-4000-8000-000000000001',
  'f1c00000-0000-4000-8000-0000000000f1', current_date, 'DRAFT');

-- 5) 应付单
INSERT INTO biz_payable
 (id, payable_no, supplier_id, source_type, source_id, source_no, due_date, period_code, status) VALUES
 -- 应付供应商 ≠ 入库单供应商 → DI-10 P0
 ('f1c00000-0000-4000-8000-000000000301','FIXDI-A1','f1c00000-0000-4000-8000-000000000001',
  'STOCK_IN','f1c00000-0000-4000-8000-000000000201','FIXDI-S1', current_date,
  to_char(current_date,'YYYY-MM'),'OPEN'),
 -- 来源入库单悬空 → DI-11 P0
 ('f1c00000-0000-4000-8000-000000000302','FIXDI-A2','f1c00000-0000-4000-8000-000000000001',
  'STOCK_IN','f1c00000-0000-4000-8000-0000000abcd2','FIXDI-SX', current_date,
  to_char(current_date,'YYYY-MM'),'OPEN');

COMMIT;
