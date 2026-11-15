-- 清理上一轮失败/中断留下的 LNK-* 夹具（只动本卡前缀，不碰别人数据）
--
-- 用法（只对 cloudpivot_test）：
--   PGPASSWORD=... psql -h 127.0.0.1 -U <role> -d cloudpivot_test -v ON_ERROR_STOP=1 \
--     -f backend/tests/linkage/cleanup_leftovers.sql
--
-- 为什么需要它：`Ctx::cleanup()` 只在用例末尾执行，断言 panic 时跑不到，于是失败那一轮
-- 会留一套夹具（编码带随机后缀，不会污染别的用例，但会累积）。平时**不用**跑；
-- 只有某一轮跑出 FAILED 之后做一次收尾。整份脚本在一个事务里，任一步外键失败则全回滚。
BEGIN;
CREATE TEMP TABLE _lnk_so ON COMMIT DROP AS
  SELECT s.id FROM biz_stock_in s JOIN biz_supplier p ON p.id = s.supplier_id
   WHERE p.supplier_code LIKE 'LNK-%';
CREATE TEMP TABLE _lnk_tk ON COMMIT DROP AS
  SELECT t.id FROM biz_stock_transfer t JOIN biz_warehouse w ON w.id = t.from_warehouse_id
   WHERE w.warehouse_code LIKE 'LNK-%';
CREATE TEMP TABLE _lnk_task ON COMMIT DROP AS
  SELECT t.id FROM biz_task t JOIN sys_user u ON u.id = t.creator_id
   WHERE u.username LIKE 'LNK-%';

-- 状态日志无外键：按单号前缀 + 任务 / 收货 / 调拨 id 清。
-- ★ 应付的状态日志 entity_no 是 `AP-…`（业务号段，不是 LNK- 前缀），其 entity_id 指向
--   biz_payable.id —— 必须在删 biz_payable 之前按 id 一起清，否则每轮 FAILED 都积一批孤儿行。
DELETE FROM biz_status_log WHERE entity_no LIKE 'LNK-%'
   OR entity_id IN (SELECT id FROM _lnk_so) OR entity_id IN (SELECT id FROM _lnk_tk)
   OR entity_id IN (SELECT id FROM _lnk_task)
   OR entity_id IN (SELECT id FROM biz_payable WHERE source_id IN (SELECT id FROM _lnk_so))
   OR entity_id IN (SELECT id FROM biz_voucher WHERE source_id IN (SELECT id FROM _lnk_so)
                     OR source_id IN (SELECT id FROM _lnk_tk) OR remark LIKE 'LNK-%');
DELETE FROM biz_payment WHERE payable_id IN (SELECT id FROM biz_payable WHERE source_id IN (SELECT id FROM _lnk_so));
DELETE FROM biz_payable WHERE source_id IN (SELECT id FROM _lnk_so);
DELETE FROM biz_voucher_entry WHERE voucher_id IN (
  SELECT id FROM biz_voucher WHERE source_id IN (SELECT id FROM _lnk_so) OR source_id IN (SELECT id FROM _lnk_tk)
    OR remark LIKE 'LNK-%');
DELETE FROM biz_voucher WHERE source_id IN (SELECT id FROM _lnk_so) OR source_id IN (SELECT id FROM _lnk_tk)
   OR remark LIKE 'LNK-%';
-- 调拨明细 → 序列号关联必须先删：biz_stock_transfer_item_serial 引用 biz_material_serial，
-- 顺序写反会让整批 DELETE 在第一处外键约束上回滚（第一版就是这个坑）。
DELETE FROM biz_stock_transfer_item_serial WHERE item_id IN
  (SELECT id FROM biz_stock_transfer_item WHERE transfer_id IN (SELECT id FROM _lnk_tk));
DELETE FROM biz_material_serial WHERE source_id IN (SELECT id FROM _lnk_so) OR source_id IN (SELECT id FROM _lnk_tk)
   OR material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'LNK-%');
DELETE FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'LNK-%');
DELETE FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'LNK-%');
DELETE FROM biz_inventory_location WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'LNK-%');
DELETE FROM biz_stock_transfer_item WHERE transfer_id IN (SELECT id FROM _lnk_tk);
DELETE FROM biz_stock_transfer WHERE id IN (SELECT id FROM _lnk_tk);
DELETE FROM biz_stock_in_item WHERE stock_in_id IN (SELECT id FROM _lnk_so);
DELETE FROM biz_stock_in WHERE id IN (SELECT id FROM _lnk_so);
DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM _lnk_task) OR biz_id IN (SELECT id FROM _lnk_so) OR biz_id IN (SELECT id FROM _lnk_tk));
DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM _lnk_task) OR biz_id IN (SELECT id FROM _lnk_so) OR biz_id IN (SELECT id FROM _lnk_tk));
DELETE FROM biz_flow_cc WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM _lnk_task) OR biz_id IN (SELECT id FROM _lnk_so) OR biz_id IN (SELECT id FROM _lnk_tk));
DELETE FROM biz_flow_instance WHERE biz_id IN (SELECT id FROM _lnk_task) OR biz_id IN (SELECT id FROM _lnk_so) OR biz_id IN (SELECT id FROM _lnk_tk)
   OR initiator_id IN (SELECT id FROM sys_user WHERE username LIKE 'LNK-%');
DELETE FROM biz_task_comment WHERE task_id IN (SELECT id FROM _lnk_task);
DELETE FROM biz_task_member WHERE task_id IN (SELECT id FROM _lnk_task);
DELETE FROM biz_task_dependency WHERE task_id IN (SELECT id FROM _lnk_task) OR depends_on_id IN (SELECT id FROM _lnk_task);
UPDATE biz_task SET parent_id = NULL WHERE id IN (SELECT id FROM _lnk_task);
DELETE FROM biz_task WHERE id IN (SELECT id FROM _lnk_task);
DELETE FROM biz_location WHERE location_code LIKE 'LNK-%';
DELETE FROM biz_material WHERE material_code LIKE 'LNK-%';
DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'LNK-%';
DELETE FROM biz_supplier WHERE supplier_code LIKE 'LNK-%';
DELETE FROM biz_unit WHERE unit_code LIKE 'LNK-%';
DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'LNK-%');
DELETE FROM sys_user WHERE username LIKE 'LNK-%';
DELETE FROM sys_role WHERE role_code LIKE 'LNK-%';
COMMIT;

-- 收尾核对：全部应为 0
SELECT 'user' AS t, count(*) FROM sys_user WHERE username LIKE 'LNK-%'
UNION ALL SELECT 'role', count(*) FROM sys_role WHERE role_code LIKE 'LNK-%'
UNION ALL SELECT 'task', count(*) FROM biz_task WHERE task_no LIKE 'LNK-%'
UNION ALL SELECT 'flow_instance', count(*) FROM biz_flow_instance WHERE biz_no LIKE 'LNK-%'
UNION ALL SELECT 'stock_in', count(*) FROM biz_stock_in WHERE in_no LIKE 'LNK-%'
UNION ALL SELECT 'transfer', count(*) FROM biz_stock_transfer WHERE transfer_no LIKE 'LNK-%'
UNION ALL SELECT 'voucher', count(*) FROM biz_voucher WHERE voucher_no LIKE 'LNK-%' OR source_no LIKE 'LNK-%'
UNION ALL SELECT 'inventory', count(*) FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'LNK-%')
UNION ALL SELECT 'inventory_location', count(*) FROM biz_inventory_location WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'LNK-%')
UNION ALL SELECT 'inventory_txn', count(*) FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'LNK-%')
UNION ALL SELECT 'serial', count(*) FROM biz_material_serial WHERE serial_no LIKE 'LNK-%'
UNION ALL SELECT 'payable', count(*) FROM biz_payable WHERE payable_no LIKE 'LNK-%'
UNION ALL SELECT 'material', count(*) FROM biz_material WHERE material_code LIKE 'LNK-%'
UNION ALL SELECT 'warehouse', count(*) FROM biz_warehouse WHERE warehouse_code LIKE 'LNK-%'
UNION ALL SELECT 'supplier', count(*) FROM biz_supplier WHERE supplier_code LIKE 'LNK-%'
UNION ALL SELECT 'location', count(*) FROM biz_location WHERE location_code LIKE 'LNK-%'
UNION ALL SELECT 'unit', count(*) FROM biz_unit WHERE unit_code LIKE 'LNK-%'
UNION ALL SELECT 'status_log', count(*) FROM biz_status_log WHERE entity_no LIKE 'LNK-%'
ORDER BY t;
