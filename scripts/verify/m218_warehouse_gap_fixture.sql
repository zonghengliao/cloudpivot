-- ============================================================================
-- m218_warehouse_gap_fixture.sql —— M2-18 仓库缺口收口 E2E 的夹具（阶段 1：库存现状）
--
-- 卡：t_659b64de（M2-18【M2·仓库】仓库缺口收口：在途 + 库龄 + 安全库存 E2E）
-- 上游：M2-08/09/10（库龄）/ M2-01..05（在途）/ M2-13/14/15（安全库存）
--
-- 只在**演练库**上跑（库名必须含 cp_ / drill / e2e / test）。可重跑：先按 M218 前缀
-- 删干净再整批插入（不删种子主数据、不动别的卡的行）。
--
-- ── 造的是什么（一格 = 物料 M218-A1 × 仓 M218-WH）──────────────────────────
--
-- | 入库批次 | 入库日期     | 数量 | 落哪个库龄档 | 在库龄报表里期望 |
-- |:--|:--|--:|:--|:--|
-- | M218-IN-0001 | 今天 − 120 天 | 10 | OVER_90   | 呆滞（> 90 天）|
-- | M218-IN-0002 | 今天 − 75 天  | 20 | D61_90    | 不呆滞（≤ 90）|
-- | M218-IN-0003 | 今天 − 45 天  | 30 | D31_60    | 不呆滞 |
-- | M218-IN-0004 | 今天 − 5 天   | 40 | D0_30     | 不呆滞 |
-- | **合计** | | **100** | | = biz_inventory.qty = 100 |
--
-- 发货在途（M2-02 过账后的落库形态，签收确认 M2-03 之前）：
--   M218-SH-0001：发 30 / 收 0  → 在途 30
--   M218-SH-0002：发 50 / 收 20 → 在途 30（部分签收仍是在途）
--   合计在途 60（只算 status = 'IN_TRANSIT'）
--
-- 未清采购订单：
--   M218-PO-0001 APPROVED 100（未收 → 计入供给）
--   M218-PO-0002 DRAFT 999（未提交 → **不计入**，否则假供给掩盖缺口）
--
-- 安全库存水位（M2-13）：min = 200 / max = 300
--
-- ── 由上面这些数推出来的期望（E2E 逐条断言）────────────────────────────────
--
--   库龄报表（今天）  : 0-30 = 40 / 31-60 = 30 / 61-90 = 20 / 90+ = 10，合计 100
--   库龄报表（今天−60）: 只剩 −120 的 10 与 −75 的 20 → 合计 30（期末快照按业务日期）
--   呆滞（> 90 天）   : 10（只有 M218-IN-0001 那层）
--   在途汇总          : 60（两行）
--   补货预计可用      : 100 − 60 + 100 = 140 → < min 200 → 建议补到 max 300 → 160
--   水位检查          : 结存 100 < 200 → 生成 1 条待办（P2，结存 > 0 未断料）
-- ============================================================================

\set ON_ERROR_STOP on

BEGIN;

-- ---------- ① 清场（只删 M218 自己的行；顺序受外键约束）----------
DELETE FROM biz_inventory_in_transit
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%')
    OR shipment_id IN (SELECT id FROM biz_shipment WHERE ship_no LIKE 'M218-%');
DELETE FROM biz_shipment_item
 WHERE shipment_id IN (SELECT id FROM biz_shipment WHERE ship_no LIKE 'M218-%')
    OR material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%');
DELETE FROM biz_shipment WHERE ship_no LIKE 'M218-%';
DELETE FROM biz_sales_order_item
 WHERE so_id IN (SELECT id FROM biz_sales_order WHERE so_no LIKE 'M218-%')
    OR material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%');
DELETE FROM biz_sales_order WHERE so_no LIKE 'M218-%';
DELETE FROM biz_stock_out_item
 WHERE stock_out_id IN (SELECT id FROM biz_stock_out WHERE out_no LIKE 'M218-%')
    OR material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%');
DELETE FROM biz_stock_out WHERE out_no LIKE 'M218-%';
DELETE FROM biz_inventory_txn
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%');
DELETE FROM biz_stock_in_item
 WHERE stock_in_id IN (SELECT id FROM biz_stock_in WHERE in_no LIKE 'M218-%');
DELETE FROM biz_stock_in WHERE in_no LIKE 'M218-%';
DELETE FROM biz_purchase_order_item
 WHERE order_id IN (SELECT id FROM biz_purchase_order WHERE order_no LIKE 'M218-%')
    OR material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%');
DELETE FROM biz_purchase_order WHERE order_no LIKE 'M218-%';
DELETE FROM biz_inventory
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%');
DELETE FROM biz_inventory_location
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%');
DELETE FROM biz_material_stock_policy
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%');
-- 待办先删：水位检查会给本格建 biz_task（doc_id = biz_inventory.id）
DELETE FROM biz_task
 WHERE doc_type = 'INVENTORY'
   AND (doc_id IN (SELECT id FROM biz_inventory
                    WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M218-%'))
        OR title LIKE '%M218-%');
DELETE FROM biz_material WHERE material_code LIKE 'M218-%';
DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'M218-%';
DELETE FROM biz_unit WHERE unit_code LIKE 'M218-%';
DELETE FROM biz_supplier WHERE supplier_code LIKE 'M218-%';
DELETE FROM biz_customer WHERE code LIKE 'M218-%';

-- ---------- ② 主数据 ----------
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status)
VALUES ('a2180000-0000-4000-8000-000000000001', 'M218-PCS', '件(M218)', 1, 1);

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, warehouse_type,
                           is_leaf, level, status)
VALUES ('a2180000-0000-4000-8000-000000000010', 'M218-WH', '收口演练仓', '/M218-WH', 'NORMAL', 1, 1, 1);

INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
VALUES ('a2180000-0000-4000-8000-000000000020', 'M218-SUP', '收口演练供应商', 1);

INSERT INTO biz_customer (id, code, name, status)
VALUES ('a2180000-0000-4000-8000-000000000030', 'M218-CUS', '收口演练客户', 'ACTIVE');

-- 物料级安全库存留 0：本卡只验「策略行」这条水位来源（三级回退的最高优先级）
INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                          safety_stock, status)
VALUES ('a2180000-0000-4000-8000-000000000101', 'M218-A1', '收口演练物料', 'M218-SPEC',
        'a2180000-0000-4000-8000-000000000001', 'RAW', 0, 1);

-- ---------- ③ 四批入库（业务日期跨 4 个库龄档）----------
-- 层日期取 biz_stock_in.in_date（不是过账时间），所以 V0424 的库龄函数能算出四档。
INSERT INTO biz_stock_in (id, in_no, in_type, is_sourceless, supplier_id, warehouse_id, in_date,
                          status, total_qty, total_amount, total_with_tax, posted_at)
VALUES ('a2180000-0000-4000-8000-000000000401', 'M218-IN-0001', 'PURCHASE', true,
        'a2180000-0000-4000-8000-000000000020', 'a2180000-0000-4000-8000-000000000010',
        CURRENT_DATE - 120, 'POSTED', 10, 100, 100, now() - interval '120 days'),
       ('a2180000-0000-4000-8000-000000000402', 'M218-IN-0002', 'PURCHASE', true,
        'a2180000-0000-4000-8000-000000000020', 'a2180000-0000-4000-8000-000000000010',
        CURRENT_DATE - 75, 'POSTED', 20, 200, 200, now() - interval '75 days'),
       ('a2180000-0000-4000-8000-000000000403', 'M218-IN-0003', 'PURCHASE', true,
        'a2180000-0000-4000-8000-000000000020', 'a2180000-0000-4000-8000-000000000010',
        CURRENT_DATE - 45, 'POSTED', 30, 300, 300, now() - interval '45 days'),
       ('a2180000-0000-4000-8000-000000000404', 'M218-IN-0004', 'PURCHASE', true,
        'a2180000-0000-4000-8000-000000000020', 'a2180000-0000-4000-8000-000000000010',
        CURRENT_DATE - 5, 'POSTED', 40, 400, 400, now() - interval '5 days');

INSERT INTO biz_stock_in_item (id, stock_in_id, line_no, material_id, unit_id, qty, price, lot_no)
VALUES ('a2180000-0000-4000-8000-000000000501', 'a2180000-0000-4000-8000-000000000401', 1,
        'a2180000-0000-4000-8000-000000000101', 'a2180000-0000-4000-8000-000000000001', 10, 10, 'M218-LOT-1'),
       ('a2180000-0000-4000-8000-000000000502', 'a2180000-0000-4000-8000-000000000402', 1,
        'a2180000-0000-4000-8000-000000000101', 'a2180000-0000-4000-8000-000000000001', 20, 10, 'M218-LOT-2'),
       ('a2180000-0000-4000-8000-000000000503', 'a2180000-0000-4000-8000-000000000403', 1,
        'a2180000-0000-4000-8000-000000000101', 'a2180000-0000-4000-8000-000000000001', 30, 10, 'M218-LOT-3'),
       ('a2180000-0000-4000-8000-000000000504', 'a2180000-0000-4000-8000-000000000404', 1,
        'a2180000-0000-4000-8000-000000000101', 'a2180000-0000-4000-8000-000000000001', 40, 10, 'M218-LOT-4');

-- 库存流水：库龄层的唯一真相（V0424 只认 biz_inventory_txn 的 IN 流水）
INSERT INTO biz_inventory_txn (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id,
                               doc_no, line_no, qty, unit_cost, amount, before_qty, after_qty,
                               period_code, remark)
SELECT 'a2180000-0000-4000-8000-000000000101', 'a2180000-0000-4000-8000-000000000010',
       'IN', 'PURCHASE_IN', 'STOCK_IN', s.id, s.in_no, 1, s.total_qty, 10, s.total_amount,
       0, s.total_qty, to_char(s.in_date, 'YYYY-MM'), 'M218 夹具：入库层'
  FROM biz_stock_in s
 WHERE s.in_no LIKE 'M218-IN-%'
 ORDER BY s.in_date;

-- 结存：100（= 四条入库层剩余之和）
INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, locked_qty, avg_cost, amount)
VALUES ('a2180000-0000-4000-8000-000000000601', 'a2180000-0000-4000-8000-000000000101',
        'a2180000-0000-4000-8000-000000000010', 100, 0, 10, 1000);

-- ---------- ④ 发货在途（两张已过账发货单）----------
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status,
                             total_amount, total_tax, total_with_tax,
                             base_total_amount, base_total_tax, base_total_with_tax)
VALUES ('a2180000-0000-4000-8000-000000000201', 'M218-SO-0001',
        'a2180000-0000-4000-8000-000000000030', CURRENT_DATE, 'APPROVED',
        800, 0, 800, 800, 0, 800);

-- 销售订单明细 + 发货单明细：在途对账（M2-05）的「发货总量」取 biz_shipment_item.qty（SSOT），
-- 没有明细的发货单会以 ORPHAN_TRANSIT 出现在对账报告里 —— 夹具必须把单据造全。
INSERT INTO biz_sales_order_item (id, so_id, line_no, material_id, qty, price, amount, delivered_qty)
VALUES ('a2180000-0000-4000-8000-000000000211', 'a2180000-0000-4000-8000-000000000201', 1,
        'a2180000-0000-4000-8000-000000000101', 80, 10, 800, 80);

INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty)
VALUES ('a2180000-0000-4000-8000-000000000202', 'M218-SH-0001', 'a2180000-0000-4000-8000-000000000201',
        'a2180000-0000-4000-8000-000000000030', 'a2180000-0000-4000-8000-000000000010',
        CURRENT_DATE - 3, 'POSTED', 30),
       ('a2180000-0000-4000-8000-000000000203', 'M218-SH-0002', 'a2180000-0000-4000-8000-000000000201',
        'a2180000-0000-4000-8000-000000000030', 'a2180000-0000-4000-8000-000000000010',
        CURRENT_DATE - 10, 'POSTED', 50);

-- 在途账：qty_in_transit 是 PG 生成列（shipped − received），夹具不写它
INSERT INTO biz_inventory_in_transit (shipment_id, material_id, warehouse_id, ship_date,
                                      shipped_qty, received_qty, status)
VALUES ('a2180000-0000-4000-8000-000000000202', 'a2180000-0000-4000-8000-000000000101',
        'a2180000-0000-4000-8000-000000000010', CURRENT_DATE - 3, 30, 0, 'IN_TRANSIT'),
       ('a2180000-0000-4000-8000-000000000203', 'a2180000-0000-4000-8000-000000000101',
        'a2180000-0000-4000-8000-000000000010', CURRENT_DATE - 10, 50, 20, 'IN_TRANSIT');

-- 发货单明细（对账的「发货总量」一侧）
INSERT INTO biz_shipment_item (id, shipment_id, so_item_id, material_id, qty)
VALUES ('a2180000-0000-4000-8000-000000000221', 'a2180000-0000-4000-8000-000000000202',
        'a2180000-0000-4000-8000-000000000211', 'a2180000-0000-4000-8000-000000000101', 30),
       ('a2180000-0000-4000-8000-000000000222', 'a2180000-0000-4000-8000-000000000203',
        'a2180000-0000-4000-8000-000000000211', 'a2180000-0000-4000-8000-000000000101', 50);

-- ---------- ⑤ 未清采购订单（一张计入、一张草稿不计入）----------
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, total_qty)
VALUES ('a2180000-0000-4000-8000-000000000301', 'M218-PO-0001',
        'a2180000-0000-4000-8000-000000000020', CURRENT_DATE, 'a2180000-0000-4000-8000-000000000010',
        'APPROVED', 100),
       ('a2180000-0000-4000-8000-000000000302', 'M218-PO-0002',
        'a2180000-0000-4000-8000-000000000020', CURRENT_DATE, 'a2180000-0000-4000-8000-000000000010',
        'DRAFT', 999);

INSERT INTO biz_purchase_order_item (id, order_id, line_no, material_id, unit_id, qty, price)
VALUES ('a2180000-0000-4000-8000-000000000311', 'a2180000-0000-4000-8000-000000000301', 1,
        'a2180000-0000-4000-8000-000000000101', 'a2180000-0000-4000-8000-000000000001', 100, 10),
       ('a2180000-0000-4000-8000-000000000312', 'a2180000-0000-4000-8000-000000000302', 1,
        'a2180000-0000-4000-8000-000000000101', 'a2180000-0000-4000-8000-000000000001', 999, 10);

-- ---------- ⑥ 安全库存水位（M2-13 策略行）----------
INSERT INTO biz_material_stock_policy (id, material_id, warehouse_id, min_qty, max_qty, status)
VALUES ('a2180000-0000-4000-8000-000000000701', 'a2180000-0000-4000-8000-000000000101',
        'a2180000-0000-4000-8000-000000000010', 200, 300, 1);

COMMIT;

-- ---------- ⑦ 夹具自检（与文件头的表逐个对上）----------
DO $$
DECLARE
    n_layer  integer;
    n_qty    numeric;
    n_tt     numeric;
    n_open   numeric;
    n_pol    integer;
BEGIN
    SELECT count(*), coalesce(sum(qty_remaining), 0) INTO n_layer, n_qty
      FROM biz_inventory_aging_layers(CURRENT_DATE, 'a2180000-0000-4000-8000-000000000101',
                                      'a2180000-0000-4000-8000-000000000010')
     WHERE qty_remaining > 0;
    SELECT coalesce(sum(qty_in_transit), 0) INTO n_tt
      FROM biz_inventory_in_transit
     WHERE material_id = 'a2180000-0000-4000-8000-000000000101' AND status = 'IN_TRANSIT';
    SELECT coalesce(sum(i.qty), 0) INTO n_open
      FROM biz_purchase_order_item i JOIN biz_purchase_order po ON po.id = i.order_id
     WHERE i.material_id = 'a2180000-0000-4000-8000-000000000101'
       AND po.status IN ('SUBMITTED', 'APPROVED', 'PARTIAL_RECEIVED') AND po.deleted_at IS NULL;
    SELECT count(*) INTO n_pol FROM biz_material_stock_policy
     WHERE material_id = 'a2180000-0000-4000-8000-000000000101' AND deleted_at IS NULL;

    IF n_layer <> 4 OR n_qty <> 100 OR n_tt <> 60 OR n_open <> 100 OR n_pol <> 1 THEN
        RAISE EXCEPTION 'M218 夹具自检失败：库龄层 %/4、层剩余合计 %/100、在途 %/60、未清订单 %/100、水位 %/1',
            n_layer, n_qty, n_tt, n_open, n_pol;
    END IF;
    RAISE NOTICE 'M218 夹具就绪（阶段 1）：4 入库层 / 结存 100 / 在途 60 / 未清订单 100 / 水位 min200 max300';
END $$;
