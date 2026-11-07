-- P0-03：清掉本卡在 145 dev 库跑 cargo test（ERP_DB_URL 指向 cloudpivot_dev）留下的
-- **测试夹具残留**。夹具仓库 = WH-W3B-1/2（`d3b0000b-*`），夹具物料 = `d3b0000c-*`，
-- 夹具库位账行 = `d3b0000f-*`。
--
-- 顺序必须与夹具 prepare() 的清理口径一致（否则被硬外键挡住）：
--   ① 先解除明细 / 序列号对夹具库位账行的引用（三张表的 location_id 都是硬外键）；
--   ② 再删夹具调拨单（明细先于单头）；
--   ③ 最后删库存账 / 库位账基线（夹具 prepare() 每次自建，不依赖残留）。
-- 只动 `d3b0000b-*` 这两个夹具仓库的行，不碰任何演练 / 业务数据（DEMO-*、WH01、WH02）。
\echo '===== 清理前：夹具残留 ====='
SELECT w.warehouse_code, m.material_code, i.qty
  FROM biz_inventory i
  JOIN biz_warehouse w ON w.id = i.warehouse_id
  JOIN biz_material m ON m.id = i.material_id
 WHERE i.warehouse_id::text LIKE 'd3b0000b-%'
 ORDER BY 1,2;

BEGIN;
UPDATE biz_stock_transfer_item SET from_location_id = NULL
 WHERE from_location_id IN (SELECT id FROM biz_inventory_location
                             WHERE warehouse_id::text LIKE 'd3b0000b-%');
UPDATE biz_stock_transfer_item SET to_location_id = NULL
 WHERE to_location_id IN (SELECT id FROM biz_inventory_location
                           WHERE warehouse_id::text LIKE 'd3b0000b-%');
UPDATE biz_material_serial SET location_id = NULL
 WHERE location_id IN (SELECT id FROM biz_inventory_location
                        WHERE warehouse_id::text LIKE 'd3b0000b-%');
DELETE FROM biz_stock_transfer_item
 WHERE transfer_id IN (SELECT id FROM biz_stock_transfer
                        WHERE from_warehouse_id::text LIKE 'd3b0000b-%'
                           OR to_warehouse_id::text LIKE 'd3b0000b-%');
DELETE FROM biz_stock_transfer
 WHERE from_warehouse_id::text LIKE 'd3b0000b-%' OR to_warehouse_id::text LIKE 'd3b0000b-%';
DELETE FROM biz_inventory_txn         WHERE warehouse_id::text LIKE 'd3b0000b-%';
-- 顺序：成本账（`biz_inventory.location_id` → `biz_inventory_location.id` 是硬外键）先于库位账
DELETE FROM biz_inventory             WHERE warehouse_id::text LIKE 'd3b0000b-%';
DELETE FROM biz_inventory_location    WHERE warehouse_id::text LIKE 'd3b0000b-%';
COMMIT;

\echo '===== 清理后：夹具残留（期望 0 行）====='
SELECT w.warehouse_code, m.material_code, i.qty
  FROM biz_inventory i
  JOIN biz_warehouse w ON w.id = i.warehouse_id
  JOIN biz_material m ON m.id = i.material_id
 WHERE i.warehouse_id::text LIKE 'd3b0000b-%';
