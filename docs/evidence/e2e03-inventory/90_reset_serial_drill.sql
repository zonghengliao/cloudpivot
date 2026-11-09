-- E2E-03 演练自清理：只动本卡自造的 E2E03-MS 序列号链数据（可重复执行）
-- 用途：让 step3（序列号管理）可以重跑，不影响任何他人数据。
-- 不动：物料主数据（E2E03-MS 本身）、仓库、库位；不动别的物料 / 别的单据。
-- 删除顺序按外键倒着来：凭证 → 收货单 → 流水 → 成本账 → 序列号 → 库位账
--   （biz_material_serial.location_id 硬外键指向 biz_inventory_location.id，所以序列号必须先删）
\set ON_ERROR_STOP on
BEGIN;
CREATE TEMP TABLE _ms ON COMMIT DROP AS
  SELECT id FROM biz_material WHERE material_code = 'E2E03-MS';
CREATE TEMP TABLE _ser ON COMMIT DROP AS
  SELECT id FROM biz_material_serial WHERE material_id IN (SELECT id FROM _ms);
CREATE TEMP TABLE _sin ON COMMIT DROP AS
  SELECT id, in_no FROM biz_stock_in WHERE remark LIKE 'E2E-03%' AND deleted_at IS NULL;

\echo '清理序列号链：凭证 → 收货单 → 流水 → 成本账 → 序列号 → 库位账'
DELETE FROM biz_voucher_entry WHERE voucher_id IN (
  SELECT id FROM biz_voucher
   WHERE (source_type IN ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP')
          AND source_id IN (SELECT id FROM _ser))
      OR (source_type = 'STOCK_IN' AND source_id IN (SELECT id FROM _sin))
);
DELETE FROM biz_voucher
 WHERE (source_type IN ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP')
        AND source_id IN (SELECT id FROM _ser))
    OR (source_type = 'STOCK_IN' AND source_id IN (SELECT id FROM _sin));
DELETE FROM biz_status_log
 WHERE entity_type = 'STOCK_IN' AND entity_id IN (SELECT id FROM _sin);
DELETE FROM biz_stock_in_item WHERE stock_in_id IN (SELECT id FROM _sin);
DELETE FROM biz_stock_in WHERE id IN (SELECT id FROM _sin);
DELETE FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM _ms);
DELETE FROM biz_inventory WHERE material_id IN (SELECT id FROM _ms);
DELETE FROM biz_material_serial WHERE material_id IN (SELECT id FROM _ms);
DELETE FROM biz_inventory_location WHERE material_id IN (SELECT id FROM _ms);
COMMIT;

\echo '--- 清理后自检（应全 0）---'
SELECT 'serials' AS what, count(*) FROM biz_material_serial s JOIN biz_material m ON m.id=s.material_id WHERE m.material_code='E2E03-MS'
UNION ALL SELECT 'inv', count(*) FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id WHERE m.material_code='E2E03-MS'
UNION ALL SELECT 'inv_loc', count(*) FROM biz_inventory_location i JOIN biz_material m ON m.id=i.material_id WHERE m.material_code='E2E03-MS'
UNION ALL SELECT 'txn', count(*) FROM biz_inventory_txn t JOIN biz_material m ON m.id=t.material_id WHERE m.material_code='E2E03-MS'
UNION ALL SELECT 'stock_in', count(*) FROM biz_stock_in WHERE remark LIKE 'E2E-03%' AND deleted_at IS NULL;
