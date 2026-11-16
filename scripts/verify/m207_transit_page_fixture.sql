-- M2-07（卡 t_10d54ac1）在途库存页**真机验收夹具**（只进演练库，跑完由 cleanup 清掉）。
--
-- 为什么直接写 SQL：本卡只读页面 + 1 个页面码迁移，没有后端写入路径要演练。
-- 真实链路（订单 → 发货单 → 过账 → 签收）已由 M2-06 的 transit_flow_tests 在 handler 层覆盖；
-- 这里要的只是一屏**有内容**的在途账，用于页面 / HTTP 验收：
--
--   明细（缺省 status=IN_TRANSIT，3 行）：
--     M207-M1 螺丝  主仓    SH1 发 100 签 40 → 在途 60（3 天前发）
--     M207-M1 螺丝  主仓    SH2 发  50 签  0 → 在途 50（10 天前发）
--     M207-M2 钢板  中转仓  SH3 发  20 签  0 → 在途 20（35 天前发 → 账龄警示）
--   另外两行是**历史行**，缺省查不到、显式 status=RECEIVED / CANCELLED 才出现：
--     M207-M3 铜管  主仓    SH1 发 10 签 10  RECEIVED
--     M207-M3 铜管  中转仓  SH2 发  5 签  0  CANCELLED
--
--   汇总（恒为未签收，按 物料×发货仓）：2 组
--     M1×主仓 在途 110（发货 150 / 已签 40，2 张单，最早 10 天前）
--     M2×中转仓 在途 20（发货 20 / 已签 0，1 张单，最早 35 天前）
--   合计：在途 130、发货单 3 张、在途行 3 行。
--
-- 幂等：全部 ON CONFLICT (id) DO UPDATE，可反复灌；清场见文件末尾注释里的 DELETE。

BEGIN;

-- ① 计量单位
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order, is_system, precision)
VALUES ('20700000-0000-4000-8000-000000000001'::uuid, 'M207-U1', '个', 1, 1, 1, 990, FALSE, 0)
ON CONFLICT (id) DO UPDATE SET unit_name = EXCLUDED.unit_name;

-- ② 仓库（主仓 / 中转仓）
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, status, sort_order, level, warehouse_path, is_leaf, allow_negative)
VALUES
  ('20700000-0000-4000-8000-000000000010'::uuid, 'M207-W1', '主仓（M207）', 'NORMAL', 1, 990, 1, '/M207-W1', 1, 0),
  ('20700000-0000-4000-8000-000000000011'::uuid, 'M207-W2', '中转仓（M207）', 'NORMAL', 1, 991, 1, '/M207-W2', 1, 0)
ON CONFLICT (id) DO UPDATE SET warehouse_name = EXCLUDED.warehouse_name;

-- ③ 物料
INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type, status, costing_method)
VALUES
  ('20700000-0000-4000-8000-000000000020'::uuid, 'M207-M1', '螺丝', 'M6×20', '20700000-0000-4000-8000-000000000001'::uuid, 'RAW', 1, 'MOVING_AVG'),
  ('20700000-0000-4000-8000-000000000021'::uuid, 'M207-M2', '钢板', '3mm', '20700000-0000-4000-8000-000000000001'::uuid, 'RAW', 1, 'MOVING_AVG'),
  ('20700000-0000-4000-8000-000000000022'::uuid, 'M207-M3', '铜管', 'Φ12', '20700000-0000-4000-8000-000000000001'::uuid, 'RAW', 1, 'MOVING_AVG')
ON CONFLICT (id) DO UPDATE SET material_name = EXCLUDED.material_name;

-- ④ 客户
INSERT INTO biz_customer (id, code, name, credit_limit, status, credit_days)
VALUES
  ('20700000-0000-4000-8000-000000000030'::uuid, 'M207-C1', '北京甲客户（M207）', 0, 'ACTIVE', 0),
  ('20700000-0000-4000-8000-000000000031'::uuid, 'M207-C2', '天津乙客户（M207）', 0, 'ACTIVE', 0)
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name;

-- ⑤ 销售订单（发货单要求 so_id 非空）
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, currency_code)
VALUES
  ('20700000-0000-4000-8000-000000000040'::uuid, 'M207-SO1', '20700000-0000-4000-8000-000000000030'::uuid, CURRENT_DATE - 40, 'APPROVED', 'CNY'),
  ('20700000-0000-4000-8000-000000000041'::uuid, 'M207-SO2', '20700000-0000-4000-8000-000000000031'::uuid, CURRENT_DATE - 40, 'APPROVED', 'CNY')
ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status;

-- ⑥ 发货单（3 张，均为 POSTED = 在途账对应的状态）
INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty)
VALUES
  ('20700000-0000-4000-8000-000000000050'::uuid, 'M207-SH1', '20700000-0000-4000-8000-000000000040'::uuid,
   '20700000-0000-4000-8000-000000000030'::uuid, '20700000-0000-4000-8000-000000000010'::uuid, CURRENT_DATE - 3, 'POSTED', 110),
  ('20700000-0000-4000-8000-000000000051'::uuid, 'M207-SH2', '20700000-0000-4000-8000-000000000040'::uuid,
   '20700000-0000-4000-8000-000000000030'::uuid, '20700000-0000-4000-8000-000000000010'::uuid, CURRENT_DATE - 10, 'POSTED', 55),
  ('20700000-0000-4000-8000-000000000052'::uuid, 'M207-SH3', '20700000-0000-4000-8000-000000000041'::uuid,
   '20700000-0000-4000-8000-000000000031'::uuid, '20700000-0000-4000-8000-000000000011'::uuid, CURRENT_DATE - 35, 'POSTED', 20)
ON CONFLICT (id) DO UPDATE SET ship_date = EXCLUDED.ship_date, status = EXCLUDED.status;

-- ⑦ 在途账（5 行：3 行在途 + 1 行已签收 + 1 行已作废）
INSERT INTO biz_inventory_in_transit (id, shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty, status, received_at, remark)
VALUES
  ('20700000-0000-4000-8000-000000000060'::uuid, '20700000-0000-4000-8000-000000000050'::uuid,
   '20700000-0000-4000-8000-000000000020'::uuid, '20700000-0000-4000-8000-000000000010'::uuid,
   CURRENT_DATE - 3, 100, 40, 'IN_TRANSIT', NULL, 'M207 夹具：部分签收'),
  ('20700000-0000-4000-8000-000000000061'::uuid, '20700000-0000-4000-8000-000000000051'::uuid,
   '20700000-0000-4000-8000-000000000020'::uuid, '20700000-0000-4000-8000-000000000010'::uuid,
   CURRENT_DATE - 10, 50, 0, 'IN_TRANSIT', NULL, 'M207 夹具：在途 10 天'),
  ('20700000-0000-4000-8000-000000000062'::uuid, '20700000-0000-4000-8000-000000000052'::uuid,
   '20700000-0000-4000-8000-000000000021'::uuid, '20700000-0000-4000-8000-000000000011'::uuid,
   CURRENT_DATE - 35, 20, 0, 'IN_TRANSIT', NULL, 'M207 夹具：在途 35 天（账龄警示）'),
  ('20700000-0000-4000-8000-000000000063'::uuid, '20700000-0000-4000-8000-000000000050'::uuid,
   '20700000-0000-4000-8000-000000000022'::uuid, '20700000-0000-4000-8000-000000000010'::uuid,
   CURRENT_DATE - 3, 10, 10, 'RECEIVED', now(), 'M207 夹具：已签收（缺省查不到）'),
  ('20700000-0000-4000-8000-000000000064'::uuid, '20700000-0000-4000-8000-000000000051'::uuid,
   '20700000-0000-4000-8000-000000000022'::uuid, '20700000-0000-4000-8000-000000000011'::uuid,
   CURRENT_DATE - 10, 5, 0, 'CANCELLED', NULL, 'M207 夹具：已作废（缺省查不到）')
ON CONFLICT (id) DO UPDATE
  SET shipped_qty = EXCLUDED.shipped_qty,
      received_qty = EXCLUDED.received_qty,
      status = EXCLUDED.status,
      received_at = EXCLUDED.received_at,
      ship_date = EXCLUDED.ship_date,
      remark = EXCLUDED.remark;

COMMIT;

-- 清场（按 id 前缀 207… 一次清完；由 drill / e2e 脚本在末尾调用）：
--   DELETE FROM biz_inventory_in_transit WHERE id::text LIKE '20700000-%';
--   DELETE FROM biz_shipment              WHERE id::text LIKE '20700000-%';
--   DELETE FROM biz_sales_order           WHERE id::text LIKE '20700000-%';
--   DELETE FROM biz_material              WHERE id::text LIKE '20700000-%';
--   DELETE FROM biz_customer              WHERE id::text LIKE '20700000-%';
--   DELETE FROM biz_warehouse             WHERE id::text LIKE '20700000-%';
--   DELETE FROM biz_unit                  WHERE id::text LIKE '20700000-%';
