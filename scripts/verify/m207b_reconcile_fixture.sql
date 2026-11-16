-- M2-07b（卡 t_4e93fa53）在途**对账**页真机验收夹具（只进演练库，跑完随库 DROP）。
--
-- 为什么写 SQL：对账是**纯读**接口（后端一行写 SQL 都没有），要造的是「账长什么样」，
-- 而不是走写入链路（发货 → 过账 → 签收已由 M2-06 的 transit_flow_tests 覆盖）。
-- 这里刻意把六种形态一次造齐（`reconcile::ComboRow::issues` 的每个分支都有一行）：
--
--   组（发货单 × 物料）          明细 Σqty   在途账                    判定
--   ───────────────────────────────────────────────────────────────────────────
--   M207B-SH1 / 螺丝 M1          100        发100 签40 → 在途60        对平（diff 0）
--   M207B-SH2 / 螺丝 M1           50        发 55 签 0 → 在途55        QTY_MISMATCH（diff -5）
--   M207B-SH3 / 钢板 M2           20        无在途行                   MISSING_TRANSIT（untracked，diff 20）
--   M207B-SH4 / 铜管 M3           10        只有一行 CANCELLED 发10    CANCELLED_ONLY（cancelled_qty 10）
--   M207B-SH5 / 铝板 M4            0        发  7 签 0 → 在途 7        ORPHAN_TRANSIT（diff -7）
--   M207B-SH6 / 螺丝 M1           30        主仓20 + 中转仓10          MULTI_WAREHOUSE（diff 0）
--
--   汇总（后端 reconcile::summarize）：checked=6 matched=1 mismatched=4 cancelled=1 untracked=1
--     item_qty(已纳入)=180  received=40  in_transit=152  diff=180-(40+152)=-12
--     缺省接口只返回 5 行（对平的 SH1 被滤掉），include_matched=true 才 6 行。
--
-- 幂等：全部 ON CONFLICT (id) DO UPDATE，可反复灌；清场见文件末尾注释。
-- 主数据 id 前缀统一 `207b…`，与 M2-07 的 `207…` 夹具互不干扰。

BEGIN;

-- ① 计量单位
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, rate, status, sort_order, is_system, precision)
VALUES ('207b0000-0000-4000-8000-000000000001'::uuid, 'M207B-U1', '个', 1, 1, 1, 980, FALSE, 0)
ON CONFLICT (id) DO UPDATE SET unit_name = EXCLUDED.unit_name;

-- ② 仓库（主仓 / 中转仓 —— MULTI_WAREHOUSE 要靠这两个）
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, status, sort_order, level, warehouse_path, is_leaf, allow_negative)
VALUES
  ('207b0000-0000-4000-8000-000000000010'::uuid, 'M207B-W1', '主仓（M207B）', 'NORMAL', 1, 980, 1, '/M207B-W1', 1, 0),
  ('207b0000-0000-4000-8000-000000000011'::uuid, 'M207B-W2', '中转仓（M207B）', 'NORMAL', 1, 981, 1, '/M207B-W2', 1, 0)
ON CONFLICT (id) DO UPDATE SET warehouse_name = EXCLUDED.warehouse_name;

-- ③ 物料（M4 只出现在在途账里 → ORPHAN_TRANSIT）
INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type, status, costing_method)
VALUES
  ('207b0000-0000-4000-8000-000000000020'::uuid, 'M207B-M1', '螺丝', 'M6×20', '207b0000-0000-4000-8000-000000000001'::uuid, 'RAW', 1, 'MOVING_AVG'),
  ('207b0000-0000-4000-8000-000000000021'::uuid, 'M207B-M2', '钢板', '3mm', '207b0000-0000-4000-8000-000000000001'::uuid, 'RAW', 1, 'MOVING_AVG'),
  ('207b0000-0000-4000-8000-000000000022'::uuid, 'M207B-M3', '铜管', 'Φ12', '207b0000-0000-4000-8000-000000000001'::uuid, 'RAW', 1, 'MOVING_AVG'),
  ('207b0000-0000-4000-8000-000000000023'::uuid, 'M207B-M4', '铝板', '5mm', '207b0000-0000-4000-8000-000000000001'::uuid, 'RAW', 1, 'MOVING_AVG')
ON CONFLICT (id) DO UPDATE SET material_name = EXCLUDED.material_name;

-- ④ 客户
INSERT INTO biz_customer (id, code, name, credit_limit, status, credit_days)
VALUES ('207b0000-0000-4000-8000-000000000030'::uuid, 'M207B-C1', '对账客户（M207B）', 0, 'ACTIVE', 0)
ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name;

-- ⑤ 销售订单 + 订单行（发货单明细的 so_item_id 是硬外键 RESTRICT，必须有这一层）
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, currency_code)
VALUES ('207b0000-0000-4000-8000-000000000040'::uuid, 'M207B-SO1',
        '207b0000-0000-4000-8000-000000000030'::uuid, CURRENT_DATE - 40, 'APPROVED', 'CNY')
ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status;

INSERT INTO biz_sales_order_item (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty)
VALUES
  ('207b0000-0000-4000-8000-000000000070'::uuid, '207b0000-0000-4000-8000-000000000040'::uuid, 1,
   '207b0000-0000-4000-8000-000000000020'::uuid, 500, 10, 5000, 13, 650, 300),
  ('207b0000-0000-4000-8000-000000000071'::uuid, '207b0000-0000-4000-8000-000000000040'::uuid, 2,
   '207b0000-0000-4000-8000-000000000021'::uuid, 100, 20, 2000, 13, 260, 20),
  ('207b0000-0000-4000-8000-000000000072'::uuid, '207b0000-0000-4000-8000-000000000040'::uuid, 3,
   '207b0000-0000-4000-8000-000000000022'::uuid, 100, 30, 3000, 13, 390, 10),
  ('207b0000-0000-4000-8000-000000000073'::uuid, '207b0000-0000-4000-8000-000000000040'::uuid, 4,
   '207b0000-0000-4000-8000-000000000023'::uuid, 100, 40, 4000, 13, 520, 0),
  ('207b0000-0000-4000-8000-000000000074'::uuid, '207b0000-0000-4000-8000-000000000040'::uuid, 5,
   '207b0000-0000-4000-8000-000000000020'::uuid, 500, 10, 5000, 13, 650, 130)
ON CONFLICT (id) DO UPDATE SET qty = EXCLUDED.qty, delivered_qty = EXCLUDED.delivered_qty;

-- ⑥ 发货单（6 张，全部 POSTED —— 对账只看 POSTED 单）
INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty)
VALUES
  ('207b0000-0000-4000-8000-000000000050'::uuid, 'M207B-SH1', '207b0000-0000-4000-8000-000000000040'::uuid,
   '207b0000-0000-4000-8000-000000000030'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid, CURRENT_DATE - 5, 'POSTED', 100),
  ('207b0000-0000-4000-8000-000000000051'::uuid, 'M207B-SH2', '207b0000-0000-4000-8000-000000000040'::uuid,
   '207b0000-0000-4000-8000-000000000030'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid, CURRENT_DATE - 12, 'POSTED', 50),
  ('207b0000-0000-4000-8000-000000000052'::uuid, 'M207B-SH3', '207b0000-0000-4000-8000-000000000040'::uuid,
   '207b0000-0000-4000-8000-000000000030'::uuid, '207b0000-0000-4000-8000-000000000011'::uuid, CURRENT_DATE - 20, 'POSTED', 20),
  ('207b0000-0000-4000-8000-000000000053'::uuid, 'M207B-SH4', '207b0000-0000-4000-8000-000000000040'::uuid,
   '207b0000-0000-4000-8000-000000000030'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid, CURRENT_DATE - 30, 'POSTED', 10),
  ('207b0000-0000-4000-8000-000000000054'::uuid, 'M207B-SH5', '207b0000-0000-4000-8000-000000000040'::uuid,
   '207b0000-0000-4000-8000-000000000030'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid, CURRENT_DATE - 8, 'POSTED', 7),
  ('207b0000-0000-4000-8000-000000000055'::uuid, 'M207B-SH6', '207b0000-0000-4000-8000-000000000040'::uuid,
   '207b0000-0000-4000-8000-000000000030'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid, CURRENT_DATE - 2, 'POSTED', 30)
ON CONFLICT (id) DO UPDATE SET ship_date = EXCLUDED.ship_date, status = EXCLUDED.status, total_qty = EXCLUDED.total_qty;

-- ⑦ 发货单明细（对账的 SSOT：Σqty 与在途账比）
INSERT INTO biz_shipment_item (id, shipment_id, so_item_id, material_id, qty, remark)
VALUES
  ('207b0000-0000-4000-8000-000000000080'::uuid, '207b0000-0000-4000-8000-000000000050'::uuid,
   '207b0000-0000-4000-8000-000000000070'::uuid, '207b0000-0000-4000-8000-000000000020'::uuid, 100, '对平组'),
  ('207b0000-0000-4000-8000-000000000081'::uuid, '207b0000-0000-4000-8000-000000000051'::uuid,
   '207b0000-0000-4000-8000-000000000070'::uuid, '207b0000-0000-4000-8000-000000000020'::uuid, 50, '数量不符组'),
  ('207b0000-0000-4000-8000-000000000082'::uuid, '207b0000-0000-4000-8000-000000000052'::uuid,
   '207b0000-0000-4000-8000-000000000071'::uuid, '207b0000-0000-4000-8000-000000000021'::uuid, 20, '没有在途行'),
  ('207b0000-0000-4000-8000-000000000083'::uuid, '207b0000-0000-4000-8000-000000000053'::uuid,
   '207b0000-0000-4000-8000-000000000072'::uuid, '207b0000-0000-4000-8000-000000000022'::uuid, 10, '整批作废'),
  ('207b0000-0000-4000-8000-000000000085'::uuid, '207b0000-0000-4000-8000-000000000055'::uuid,
   '207b0000-0000-4000-8000-000000000074'::uuid, '207b0000-0000-4000-8000-000000000020'::uuid, 30, '多仓在途')
ON CONFLICT (id) DO UPDATE SET qty = EXCLUDED.qty, remark = EXCLUDED.remark;

-- ⑧ 在途账（6 行：对平 / 数量不符 / 整批作废 / 孤儿 / 多仓两行）
--    `qty_in_transit` 是 PG 生成列（V0420），这里绝不写它。
INSERT INTO biz_inventory_in_transit (id, shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty, status, received_at, remark)
VALUES
  ('207b0000-0000-4000-8000-000000000060'::uuid, '207b0000-0000-4000-8000-000000000050'::uuid,
   '207b0000-0000-4000-8000-000000000020'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid,
   CURRENT_DATE - 5, 100, 40, 'IN_TRANSIT', NULL, 'M207B 夹具：对平组（发100 签40 在途60）'),
  ('207b0000-0000-4000-8000-000000000061'::uuid, '207b0000-0000-4000-8000-000000000051'::uuid,
   '207b0000-0000-4000-8000-000000000020'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid,
   CURRENT_DATE - 12, 55, 0, 'IN_TRANSIT', NULL, 'M207B 夹具：在途账记 55，明细只有 50'),
  ('207b0000-0000-4000-8000-000000000063'::uuid, '207b0000-0000-4000-8000-000000000053'::uuid,
   '207b0000-0000-4000-8000-000000000022'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid,
   CURRENT_DATE - 30, 10, 0, 'CANCELLED', NULL, 'M207B 夹具：整批作废'),
  ('207b0000-0000-4000-8000-000000000064'::uuid, '207b0000-0000-4000-8000-000000000054'::uuid,
   '207b0000-0000-4000-8000-000000000023'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid,
   CURRENT_DATE - 8, 7, 0, 'IN_TRANSIT', NULL, 'M207B 夹具：在途账有货但明细没有'),
  ('207b0000-0000-4000-8000-000000000065'::uuid, '207b0000-0000-4000-8000-000000000055'::uuid,
   '207b0000-0000-4000-8000-000000000020'::uuid, '207b0000-0000-4000-8000-000000000010'::uuid,
   CURRENT_DATE - 2, 20, 0, 'IN_TRANSIT', NULL, 'M207B 夹具：多仓之一（主仓 20）'),
  ('207b0000-0000-4000-8000-000000000066'::uuid, '207b0000-0000-4000-8000-000000000055'::uuid,
   '207b0000-0000-4000-8000-000000000020'::uuid, '207b0000-0000-4000-8000-000000000011'::uuid,
   CURRENT_DATE - 2, 10, 0, 'IN_TRANSIT', NULL, 'M207B 夹具：多仓之二（中转仓 10）')
ON CONFLICT (id) DO UPDATE
  SET shipped_qty = EXCLUDED.shipped_qty,
      received_qty = EXCLUDED.received_qty,
      status = EXCLUDED.status,
      received_at = EXCLUDED.received_at,
      ship_date = EXCLUDED.ship_date,
      warehouse_id = EXCLUDED.warehouse_id,
      remark = EXCLUDED.remark;

COMMIT;

-- 清场（按 id 前缀 207b… 一次清完；演练库整体 DROP 也可以）：
--   DELETE FROM biz_inventory_in_transit WHERE id::text LIKE '207b0000-%';
--   DELETE FROM biz_shipment_item          WHERE id::text LIKE '207b0000-%';
--   DELETE FROM biz_shipment               WHERE id::text LIKE '207b0000-%';
--   DELETE FROM biz_sales_order_item       WHERE id::text LIKE '207b0000-%';
--   DELETE FROM biz_sales_order            WHERE id::text LIKE '207b0000-%';
--   DELETE FROM biz_material               WHERE id::text LIKE '207b0000-%';
--   DELETE FROM biz_customer               WHERE id::text LIKE '207b0000-%';
--   DELETE FROM biz_warehouse              WHERE id::text LIKE '207b0000-%';
--   DELETE FROM biz_unit                   WHERE id::text LIKE '207b0000-%';
