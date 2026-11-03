-- PERIOD-CLOSE 真机验证：把演示数据重置回「三张单都挡路」的初始状态
-- 用途：重复跑 period-close-ui.mjs phase 1 时先把现场复原
BEGIN;
DELETE FROM sys_notice_receiver WHERE notice_id IN (SELECT id FROM sys_notice WHERE biz_type='period_remind');
DELETE FROM sys_notice_delivery WHERE notice_id IN (SELECT id FROM sys_notice WHERE biz_type='period_remind');
DELETE FROM sys_notice WHERE biz_type='period_remind';

UPDATE biz_stock_in SET status='DRAFT', cancelled_at=NULL
 WHERE in_no='PC-IN-203603-001';
UPDATE biz_purchase_order SET status='SUBMITTED'
 WHERE order_no='PC-PO-203603-001';
UPDATE biz_voucher SET status='DRAFT'
 WHERE voucher_no='PC-V-203603-001';

-- 期间回到 OPEN（上一轮 phase 4 结账后又反结账，正常是 OPEN；这里兜底）
UPDATE biz_accounting_period SET status='OPEN', closed_at=NULL, closed_by=NULL
 WHERE period_code='2036-03' AND status='CLOSED';
COMMIT;
