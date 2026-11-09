-- E2E-07 重置：只删本卡自造数据（contract_name like 'E2E07-%' 及其派生单据）
-- 不动任何别人的演练数据；不 DROP / TRUNCATE。
\set ON_ERROR_STOP on
BEGIN;

CREATE TEMP TABLE e7_contract ON COMMIT DROP AS
  SELECT id, contract_no FROM biz_contract WHERE contract_name LIKE 'E2E07-%';

CREATE TEMP TABLE e7_po ON COMMIT DROP AS
  SELECT id FROM biz_purchase_order WHERE contract_id IN (SELECT id FROM e7_contract);

CREATE TEMP TABLE e7_so ON COMMIT DROP AS
  SELECT id FROM biz_sales_order WHERE contract_id IN (SELECT id FROM e7_contract);

CREATE TEMP TABLE e7_inst ON COMMIT DROP AS
  SELECT id FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM e7_contract);

CREATE TEMP TABLE e7_notice ON COMMIT DROP AS
  SELECT n.id FROM sys_notice n
   WHERE n.biz_id IN (SELECT id::text FROM e7_contract)
      OR EXISTS (SELECT 1 FROM e7_contract c
                  WHERE n.content LIKE '%' || c.contract_no || '%'
                     OR n.title LIKE '%' || c.contract_no || '%');

DELETE FROM sys_notice_delivery WHERE notice_id IN (SELECT id FROM e7_notice) OR receiver_id IN (SELECT id FROM sys_notice_receiver WHERE notice_id IN (SELECT id FROM e7_notice));
DELETE FROM sys_notice_receiver WHERE notice_id IN (SELECT id FROM e7_notice);
DELETE FROM sys_notice WHERE id IN (SELECT id FROM e7_notice);

DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM e7_inst);
DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM e7_inst);
DELETE FROM biz_flow_cc WHERE instance_id IN (SELECT id FROM e7_inst);
DELETE FROM biz_task WHERE flow_instance_id IN (SELECT id FROM e7_inst);
UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE id IN (SELECT id FROM e7_inst);
DELETE FROM biz_flow_instance WHERE id IN (SELECT id FROM e7_inst);

DELETE FROM biz_purchase_order_item WHERE order_id IN (SELECT id FROM e7_po);
DELETE FROM biz_purchase_order WHERE id IN (SELECT id FROM e7_po);
DELETE FROM biz_sales_order_item WHERE so_id IN (SELECT id FROM e7_so);
DELETE FROM biz_sales_order WHERE id IN (SELECT id FROM e7_so);

DELETE FROM biz_status_log WHERE entity_type = 'CONTRACT' AND entity_id IN (SELECT id FROM e7_contract);
DELETE FROM sys_change_log WHERE resource = 'biz_contract' AND resource_id IN (SELECT id::text FROM e7_contract);
DELETE FROM biz_doc_attachment WHERE doc_type = 'CONTRACT' AND doc_id IN (SELECT id FROM e7_contract);
DELETE FROM biz_reminder_log WHERE doc_type = 'CONTRACT' AND doc_id IN (SELECT id FROM e7_contract);
DELETE FROM biz_contract WHERE id IN (SELECT id FROM e7_contract);

COMMIT;
SELECT 'reset_ok' AS status, (SELECT count(*) FROM biz_contract WHERE contract_name LIKE 'E2E07-%') AS remaining;
