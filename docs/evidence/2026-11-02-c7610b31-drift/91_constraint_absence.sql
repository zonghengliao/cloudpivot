-- ============================================================
-- 91_constraint_absence.sql —— 「约束被人 drop 掉之后」才可能出现的巡检项自证
--   DI-04 / DI-05 / DI-09 的正常状态下被 DB 约束拦住（恒 0 行），
--   本脚本在事务内临时摘掉约束、构造缺陷、跑三条巡检、然后 ROLLBACK（不落任何痕迹）。
-- ============================================================
BEGIN;

ALTER TABLE biz_purchase_order DROP CONSTRAINT IF EXISTS ck_biz_po_contract_pair;
ALTER TABLE biz_purchase_order DROP CONSTRAINT IF EXISTS ck_biz_po_purchase_type;
ALTER TABLE biz_purchase_order ALTER COLUMN supplier_id DROP NOT NULL;
ALTER TABLE biz_stock_in      DROP CONSTRAINT IF EXISTS ck_biz_si_supplier;
-- 顺带验证 DI-14：把巡检自己的去重护栏拆了
DROP INDEX IF EXISTS uk_biz_drift_open;

INSERT INTO biz_purchase_order
 (id, order_no, supplier_id, order_date, warehouse_id, status, purchase_type, contract_id, contract_no_snapshot)
VALUES
 -- CONTRACT 但无 contract_id → DI-04
 ('f1c00000-0000-4000-8000-0000000001a1','FIXDI-PA-1','f1c00000-0000-4000-8000-000000000001',
  current_date,'f1c00000-0000-4000-8000-0000000000f1','DRAFT','CONTRACT',NULL,NULL),
 -- SPOT 但无 supplier_id → DI-05
 ('f1c00000-0000-4000-8000-0000000001a2','FIXDI-PA-2',NULL,
  current_date,'f1c00000-0000-4000-8000-0000000000f1','DRAFT','SPOT',NULL,NULL);

INSERT INTO biz_stock_in
 (id, in_no, in_type, supplier_id, warehouse_id, in_date, status)
VALUES
 -- PURCHASE 但无 supplier_id → DI-09
 ('f1c00000-0000-4000-8000-0000000002a1','FIXDI-SA-1','PURCHASE',NULL,
  'f1c00000-0000-4000-8000-0000000000f1', current_date, 'DRAFT');

\echo '--- DI-04（CONTRACT 缺 contract_id）'
SELECT issue_code, entity_no, snapshot_value, truth_value, severity FROM di.c_di04;
\echo '--- DI-05（SPOT 缺 supplier_id）'
SELECT issue_code, entity_no, snapshot_value, truth_value, severity FROM di.c_di05;
\echo '--- DI-09（采购入库缺 supplier_id）'
SELECT issue_code, entity_no, snapshot_value, truth_value, severity FROM di.c_di09;
\echo '--- DI-14（结构护栏缺失）'
SELECT issue_code, snapshot_value, severity FROM di.c_di14;

ROLLBACK;
