-- ============================================================================
-- m208_inventory_aging_fixture.sql —— 库龄（M2-08）真接口 / 真机验收夹具
--
-- 幂等：先删本夹具的业务数据（按 `M208H-` 单号前缀 + 固定 UUID 主数据），再重建。
-- 只动自己的行，不碰库里任何既有数据。
--
-- 造的形态（FIFO 一眼能看出来）：
--   层 A  100 件  业务日期 2026-09-01  过账 09-02  ← 被吃满
--   层 B  200 件  业务日期 2026-10-15  过账 10-16  ← 剩 120（吃 80）
--   层 C   50 件  业务日期 2026-11-10  过账 11-14  ← 剩 50（补录：过账晚于业务日期）
--   出库  180 件  业务日期 2026-10-20  过账 11-12  ← 补录：过账晚于业务日期
--   ⇒ 基准日 2026-11-16：B 剩 120（库龄 32 天）、C 剩 50（库龄 6 天），Σ = 170 = biz_inventory.qty
--   ⇒ 基准日 2026-10-31：C 还没入库；出库已按业务日期算进来 ⇒ B 剩 120
--   ⇒ 基准日 2026-10-18：出库还没发生 ⇒ A 100 + B 200
--
-- 跑法：
--   psql -h <host> -U cloudpivot -d <drill_db> -v ON_ERROR_STOP=1 -f scripts/verify/m208_inventory_aging_fixture.sql
-- 清场（可选）：
--   psql ... -c "DELETE FROM biz_inventory_txn WHERE doc_no LIKE 'M208H-%';
--                DELETE FROM biz_stock_in WHERE in_no LIKE 'M208H-%';
--                DELETE FROM biz_stock_out WHERE out_no LIKE 'M208H-%';
--                DELETE FROM biz_inventory WHERE material_id =
--                  (SELECT id FROM biz_material WHERE material_code = 'M208H-M');"
-- ============================================================================

-- ---------- 清场（幂等） ----------
DELETE FROM biz_inventory_txn WHERE doc_no LIKE 'M208H-%';
DELETE FROM biz_stock_in      WHERE in_no  LIKE 'M208H-%';
DELETE FROM biz_stock_out     WHERE out_no LIKE 'M208H-%';
DELETE FROM biz_inventory     WHERE material_id IN (SELECT id FROM biz_material WHERE material_code = 'M208H-M');

-- ---------- 主数据（固定 UUID，可重复执行） ----------
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
VALUES ('20800000-0000-4000-8000-000000000001', 'M208H-PCS', '件', 1, NULL, 1.000000, 1, 1)
ON CONFLICT DO NOTHING;

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative)
VALUES ('20800000-0000-4000-8000-000000000002', 'M208H-WH', '库龄验收仓', '/20800000-0000-4000-8000-000000000002/', 1, 1, 1, 0)
ON CONFLICT DO NOTHING;

INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
VALUES ('20800000-0000-4000-8000-000000000003', 'M208H-M', '库龄验收物料',
        '20800000-0000-4000-8000-000000000001', 1, false)
ON CONFLICT DO NOTHING;

-- ---------- 三张入库单（业务日期 09-01 / 10-15 / 11-10；无源单 ⇒ is_sourceless） ----------
INSERT INTO biz_stock_in (id, in_no, in_type, warehouse_id, in_date, status, total_qty, is_sourceless) VALUES
 ('20800000-0000-4000-8000-0000000000a1', 'M208H-IN-A', 'OTHER', '20800000-0000-4000-8000-000000000002', '2026-09-01', 'POSTED', 100, TRUE),
 ('20800000-0000-4000-8000-0000000000a2', 'M208H-IN-B', 'OTHER', '20800000-0000-4000-8000-000000000002', '2026-10-15', 'POSTED', 200, TRUE),
 ('20800000-0000-4000-8000-0000000000a3', 'M208H-IN-C', 'OTHER', '20800000-0000-4000-8000-000000000002', '2026-11-10', 'POSTED',  50, TRUE);

INSERT INTO biz_stock_in_item
 (stock_in_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate,
  amount, tax_amount, with_tax_amount, cost_price, lot_no) VALUES
 ('20800000-0000-4000-8000-0000000000a1', 1, '20800000-0000-4000-8000-000000000003',
  '20800000-0000-4000-8000-000000000001', 100, 10, 10, 0, 1000, 0, 1000, 10, 'M208H-LOT-A'),
 ('20800000-0000-4000-8000-0000000000a2', 1, '20800000-0000-4000-8000-000000000003',
  '20800000-0000-4000-8000-000000000001', 200, 10, 10, 0, 2000, 0, 2000, 10, 'M208H-LOT-B'),
 ('20800000-0000-4000-8000-0000000000a3', 1, '20800000-0000-4000-8000-000000000003',
  '20800000-0000-4000-8000-000000000001',  50, 10, 10, 0,  500, 0,  500, 10, 'M208H-LOT-C');

-- ---------- 一张出库单（业务日期 10-20，过账时间 11-12 = 补录） ----------
INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, total_qty)
VALUES ('20800000-0000-4000-8000-0000000000b1', 'M208H-OUT-X', 'OTHER',
        '20800000-0000-4000-8000-000000000002', '2026-10-20', 'POSTED', 180);

-- ---------- 库存流水（三笔 IN + 一笔 OUT；过账时刻与业务日期故意错开） ----------
INSERT INTO biz_inventory_txn
 (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
  qty, unit_cost, amount, before_qty, before_amount, before_cost,
  after_qty, after_amount, after_cost, period_code, created_at) VALUES
 ('20800000-0000-4000-8000-0000000000c1', '20800000-0000-4000-8000-000000000003',
  '20800000-0000-4000-8000-000000000002', 'IN', 'PURCHASE_IN', 'STOCK_IN',
  '20800000-0000-4000-8000-0000000000a1', 'M208H-IN-A', 1, 100, 10, 1000, 0, 0, 0, 100, 1000, 10,
  '2026-09', '2026-09-02T02:00:00Z'),
 ('20800000-0000-4000-8000-0000000000c2', '20800000-0000-4000-8000-000000000003',
  '20800000-0000-4000-8000-000000000002', 'IN', 'PURCHASE_IN', 'STOCK_IN',
  '20800000-0000-4000-8000-0000000000a2', 'M208H-IN-B', 1, 200, 10, 2000, 0, 0, 0, 300, 3000, 10,
  '2026-10', '2026-10-16T02:00:00Z'),
 ('20800000-0000-4000-8000-0000000000c3', '20800000-0000-4000-8000-000000000003',
  '20800000-0000-4000-8000-000000000002', 'IN', 'PURCHASE_IN', 'STOCK_IN',
  '20800000-0000-4000-8000-0000000000a3', 'M208H-IN-C', 1,  50, 10,  500, 0, 0, 0, 350, 3500, 10,
  '2026-11', '2026-11-14T02:00:00Z'),
 ('20800000-0000-4000-8000-0000000000c4', '20800000-0000-4000-8000-000000000003',
  '20800000-0000-4000-8000-000000000002', 'OUT', 'SALE_OUT', 'STOCK_OUT',
  '20800000-0000-4000-8000-0000000000b1', 'M208H-OUT-X', 1, 180, 10, 1800, 0, 0, 0, 170, 1700, 10,
  '2026-10', '2026-11-12T02:00:00Z');

-- ---------- 结存账（Σ层剩余必须等于它：170 = 350 - 180） ----------
INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount, last_txn_at)
VALUES ('20800000-0000-4000-8000-0000000000d1', '20800000-0000-4000-8000-000000000003',
        '20800000-0000-4000-8000-000000000002', 170, 0, 10, 1700, '2026-11-14T02:00:00Z');
