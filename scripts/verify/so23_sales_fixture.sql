-- =============================================================================
-- SO-23 正向夹具：一条**完整且自洽**的销售链（订单→发货→出库→应收→收款→发票→凭证）
-- =============================================================================
-- 用途：给 S1–S8 八条销售不变量造「**非空**」的正向覆盖 —— 干净库上 8 条会
--       绿色地空跑（0 行），那是「没查」不是「查过且没问题」。本夹具让每条
--       都真的扫到行并判定通过。
--
-- 口径说明（别把夹具当生产数据）：
--   · 三本账（biz_inventory / biz_inventory_location / biz_inventory_txn）在这里
--     是**成套直写**的：期初 IN 20、销售出库 OUT 10/5，三本账与流水逐笔对齐 ——
--     等价于 inventory_writer.rs 过账后的结果形态。产品代码禁止直写三本账，
--     夹具是「复刻正确结果」，不是「绕过写入入口」（生产链路走 SO-24 的 API 全链）。
--   · 金额按 ARCH-7 §8.1 的科目映射：出库成本 借6401/贷1405、收入 借1122/
--     贷5001+贷2221.02、收款 借1002/贷1122；期初 借1405/贷1002 让 B2
--     （库存金额 = 1405 余额）也能成立。
--   · 一张订单 2 行、拆 2 张发货单 / 2 张出库单，覆盖「部分发货 + 多次出库」。
--
-- 幂等：开头按固定 UUID / 单号前缀 SO23- 清场，可反复执行。
-- =============================================================================
\set ON_ERROR_STOP on

BEGIN;

-- ---------- 0. 清场（只碰 SO23- 前缀的夹具行） ----------
DELETE FROM biz_voucher_entry WHERE voucher_id IN (
    SELECT id FROM biz_voucher WHERE voucher_no LIKE 'SO23-V-%');
DELETE FROM biz_voucher WHERE voucher_no LIKE 'SO23-V-%';
DELETE FROM biz_invoice WHERE invoice_no LIKE 'SO23-IV-%';
DELETE FROM biz_receipt_allocation WHERE receipt_id IN (
    SELECT id FROM biz_receipt WHERE receipt_no LIKE 'SO23-RC-%');
DELETE FROM biz_receipt WHERE receipt_no LIKE 'SO23-RC-%';
DELETE FROM biz_receivable WHERE ar_no LIKE 'SO23-AR-%';
DELETE FROM biz_inventory_txn WHERE doc_no LIKE 'SO23-%' OR doc_no LIKE '%SO23-%';
DELETE FROM biz_inventory_location WHERE material_id IN (
    'd1370003-0000-4000-8000-000000000001'::uuid, 'd1370003-0000-4000-8000-000000000002'::uuid);
DELETE FROM biz_inventory WHERE material_id IN (
    'd1370003-0000-4000-8000-000000000001'::uuid, 'd1370003-0000-4000-8000-000000000002'::uuid);
DELETE FROM biz_stock_out_item WHERE stock_out_id IN (
    SELECT id FROM biz_stock_out WHERE out_no LIKE 'SO23-OUT-%');
DELETE FROM biz_stock_out WHERE out_no LIKE 'SO23-OUT-%';
DELETE FROM biz_shipment_item WHERE shipment_id IN (
    SELECT id FROM biz_shipment WHERE ship_no LIKE 'SO23-SH-%');
DELETE FROM biz_shipment WHERE ship_no LIKE 'SO23-SH-%';
DELETE FROM biz_sales_order_item WHERE so_id IN (
    SELECT id FROM biz_sales_order WHERE so_no LIKE 'SO23-SO-%');
DELETE FROM biz_sales_order WHERE so_no LIKE 'SO23-SO-%';
DELETE FROM biz_customer WHERE code = 'SO23-CUS';

-- ---------- 1. 客户 ----------
INSERT INTO biz_customer (id, code, name, credit_limit, credit_days, status, remark)
VALUES ('d0230000-0000-4000-8000-000000000001'::uuid, 'SO23-CUS', 'SO23 测试客户A',
        100000, 30, 'ACTIVE', 'SO-23 销售不变量正向夹具');

-- ---------- 2. 销售订单（2 行；头 = 明细之和） ----------
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status,
                             total_amount, total_tax, total_with_tax, remark)
VALUES ('d0231000-0000-4000-8000-000000000001'::uuid, 'SO23-SO-0001',
        'd0230000-0000-4000-8000-000000000001'::uuid, DATE '2026-11-05', 'OUTBOUND',
        2000.0000, 260.0000, 2260.0000, 'SO-23 正向夹具：2 行、分两次发货');

INSERT INTO biz_sales_order_item (id, so_id, line_no, material_id, qty, price,
                                  amount, tax_rate, tax_amount, delivered_qty)
VALUES
  ('d0231100-0000-4000-8000-000000000001'::uuid, 'd0231000-0000-4000-8000-000000000001'::uuid, 1,
   'd1370003-0000-4000-8000-000000000001'::uuid, 10.0000, 100.000000, 1000.0000, 13.0000, 130.0000, 10.0000),
  ('d0231100-0000-4000-8000-000000000002'::uuid, 'd0231000-0000-4000-8000-000000000001'::uuid, 2,
   'd1370003-0000-4000-8000-000000000002'::uuid,  5.0000, 200.000000, 1000.0000, 13.0000, 130.0000,  5.0000);

-- ---------- 3. 发货单（2 张，各对应 1 张出库单） ----------
INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date,
                          status, total_qty, stock_out_id, remark)
VALUES
  ('d0232000-0000-4000-8000-000000000001'::uuid, 'SO23-SH-0001',
   'd0231000-0000-4000-8000-000000000001'::uuid, 'd0230000-0000-4000-8000-000000000001'::uuid,
   'd1370001-0000-4000-8000-000000000002'::uuid, DATE '2026-11-05', 'POSTED', 10.0000,
   'd0233000-0000-4000-8000-000000000001'::uuid, '第 1 行全发'),
  ('d0232000-0000-4000-8000-000000000002'::uuid, 'SO23-SH-0002',
   'd0231000-0000-4000-8000-000000000001'::uuid, 'd0230000-0000-4000-8000-000000000001'::uuid,
   'd1370001-0000-4000-8000-000000000002'::uuid, DATE '2026-11-05', 'POSTED', 5.0000,
   'd0233000-0000-4000-8000-000000000002'::uuid, '第 2 行全发');

INSERT INTO biz_shipment_item (id, shipment_id, so_item_id, material_id, qty, location_id)
VALUES
  ('d0232100-0000-4000-8000-000000000001'::uuid, 'd0232000-0000-4000-8000-000000000001'::uuid,
   'd0231100-0000-4000-8000-000000000001'::uuid, 'd1370003-0000-4000-8000-000000000001'::uuid,
   10.0000, 'd1370002-0000-4000-8000-000000000001'::uuid),
  ('d0232100-0000-4000-8000-000000000002'::uuid, 'd0232000-0000-4000-8000-000000000002'::uuid,
   'd0231100-0000-4000-8000-000000000002'::uuid, 'd1370003-0000-4000-8000-000000000002'::uuid,
    5.0000, 'd1370002-0000-4000-8000-000000000001'::uuid);

-- ---------- 4. 销售出库单（source_type='SALES'、out_type='OTHER'，两个维度别混） ----------
INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status,
                           total_qty, total_amount, voucher_id, source_type, source_id,
                           posted_at, posted_by, remark)
VALUES
  ('d0233000-0000-4000-8000-000000000001'::uuid, 'SO23-OUT-0001', 'OTHER',
   'd1370001-0000-4000-8000-000000000002'::uuid, DATE '2026-11-05', 'POSTED',
   10.0000, 1000.0000, 'd0237000-0000-4000-8000-000000000002'::uuid, 'SALES',
   'd0232000-0000-4000-8000-000000000001'::uuid, now(), NULL, '销售出库·成本结转 1000'),
  ('d0233000-0000-4000-8000-000000000002'::uuid, 'SO23-OUT-0002', 'OTHER',
   'd1370001-0000-4000-8000-000000000002'::uuid, DATE '2026-11-05', 'POSTED',
    5.0000, 1000.0000, 'd0237000-0000-4000-8000-000000000003'::uuid, 'SALES',
   'd0232000-0000-4000-8000-000000000002'::uuid, now(), NULL, '销售出库·成本结转 1000');

INSERT INTO biz_stock_out_item (id, stock_out_id, line_no, material_id, unit_id, qty,
                                unit_cost, amount, location_id)
VALUES
  ('d0233100-0000-4000-8000-000000000001'::uuid, 'd0233000-0000-4000-8000-000000000001'::uuid, 1,
   'd1370003-0000-4000-8000-000000000001'::uuid, '798472e4-30b4-4101-8991-1998aef8f719'::uuid,
   10.0000, 100.000000, 1000.0000, 'd1370002-0000-4000-8000-000000000001'::uuid),
  ('d0233100-0000-4000-8000-000000000002'::uuid, 'd0233000-0000-4000-8000-000000000002'::uuid, 1,
   'd1370003-0000-4000-8000-000000000002'::uuid, '798472e4-30b4-4101-8991-1998aef8f719'::uuid,
    5.0000, 200.000000, 1000.0000, 'd1370002-0000-4000-8000-000000000001'::uuid);

-- ---------- 5. 三本账（期初 IN 20 → 销售出库 OUT 10 / OUT 5） ----------
-- ⚠️ 顺序有 FK：`biz_inventory.location_id` → **biz_inventory_location.id**（不是
--    biz_location.id，实测 V0203 的外键定义）。先建库位账，成本账才能引用它。
INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
VALUES
  ('d0239000-0000-4000-8000-000000000001'::uuid,
   'd1370003-0000-4000-8000-000000000001'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
   'd1370002-0000-4000-8000-000000000001'::uuid, 10.0000),
  ('d0239000-0000-4000-8000-000000000002'::uuid,
   'd1370003-0000-4000-8000-000000000002'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
   'd1370002-0000-4000-8000-000000000001'::uuid, 15.0000);

INSERT INTO biz_inventory (material_id, warehouse_id, location_id, qty, avg_cost, amount)
VALUES
  ('d1370003-0000-4000-8000-000000000001'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
   'd0239000-0000-4000-8000-000000000001'::uuid, 10.0000, 100.000000, 1000.0000),
  ('d1370003-0000-4000-8000-000000000002'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
   'd0239000-0000-4000-8000-000000000002'::uuid, 15.0000, 200.000000, 3000.0000);

INSERT INTO biz_inventory_txn (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id,
                               doc_no, line_no, qty, unit_cost, amount, period_code, remark)
VALUES
  ('d1370003-0000-4000-8000-000000000001'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
   'IN', 'OPENING', 'OPENING', 'd0238000-0000-4000-8000-000000000001'::uuid,
   'SO23-OPEN-0001', 1, 20.0000, 100.000000, 2000.0000, '2026-11', '期初建账'),
  ('d1370003-0000-4000-8000-000000000002'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
   'IN', 'OPENING', 'OPENING', 'd0238000-0000-4000-8000-000000000002'::uuid,
   'SO23-OPEN-0002', 1, 20.0000, 200.000000, 4000.0000, '2026-11', '期初建账'),
  ('d1370003-0000-4000-8000-000000000001'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
   'OUT', 'OTHER_OUT', 'STOCK_OUT', 'd0233000-0000-4000-8000-000000000001'::uuid,
   'SO23-OUT-0001', 1, 10.0000, 100.000000, 1000.0000, '2026-11', '销售出库'),
  ('d1370003-0000-4000-8000-000000000002'::uuid, 'd1370001-0000-4000-8000-000000000002'::uuid,
   'OUT', 'OTHER_OUT', 'STOCK_OUT', 'd0233000-0000-4000-8000-000000000002'::uuid,
   'SO23-OUT-0002', 1, 5.0000, 200.000000, 1000.0000, '2026-11', '销售出库');

-- ---------- 6. 应收（SALES_OUT 来源；AR1 已收 565、AR2 未收） ----------
INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no,
                            amount, received_amount, unsettled_amount, credit_days, due_date,
                            period_code, status, voucher_id, remark)
VALUES
  ('d0234000-0000-4000-8000-000000000001'::uuid, 'SO23-AR-0001',
   'd0230000-0000-4000-8000-000000000001'::uuid, 'SALES_OUT',
   'd0233000-0000-4000-8000-000000000001'::uuid, 'SO23-OUT-0001',
   1130.0000, 565.0000, 565.0000, 30, DATE '2026-12-05', '2026-11', 'PARTIAL',
   'd0237000-0000-4000-8000-000000000004'::uuid, '出库自动挂账'),
  ('d0234000-0000-4000-8000-000000000002'::uuid, 'SO23-AR-0002',
   'd0230000-0000-4000-8000-000000000001'::uuid, 'SALES_OUT',
   'd0233000-0000-4000-8000-000000000002'::uuid, 'SO23-OUT-0002',
   1130.0000, 0.0000, 1130.0000, 30, DATE '2026-12-05', '2026-11', 'OPEN',
   NULL, '出库自动挂账');

-- ---------- 7. 收款 + 核销（565 全核销到 AR1） ----------
INSERT INTO biz_receipt (id, receipt_no, customer_id, receipt_date, amount, allocated_amount,
                         method, bank_account, status, voucher_id, remark)
VALUES ('d0235000-0000-4000-8000-000000000001'::uuid, 'SO23-RC-0001',
        'd0230000-0000-4000-8000-000000000001'::uuid, DATE '2026-11-06',
        565.0000, 565.0000, 'BANK_TRANSFER', '6222-0001', 'POSTED',
        'd0237000-0000-4000-8000-000000000005'::uuid, '收款核销 AR1');

INSERT INTO biz_receipt_allocation (id, receipt_id, receivable_id, amount, remark)
VALUES ('d0235100-0000-4000-8000-000000000001'::uuid,
        'd0235000-0000-4000-8000-000000000001'::uuid,
        'd0234000-0000-4000-8000-000000000001'::uuid,
        565.0000, '核销 AR1');

-- ---------- 8. 发票（OUT 正票；关联 AR1 与收入凭证） ----------
INSERT INTO biz_invoice (id, invoice_no, invoice_type, customer_id, invoice_date,
                         amount, tax_amount, total_with_tax, tax_rate, status,
                         receivable_id, source_type, source_no, voucher_id, remark)
VALUES ('d0236000-0000-4000-8000-000000000001'::uuid, 'SO23-IV-0001', 'OUT',
        'd0230000-0000-4000-8000-000000000001'::uuid, DATE '2026-11-06',
        1000.0000, 130.0000, 1130.0000, 0.1300, 'ISSUED',
        'd0234000-0000-4000-8000-000000000001'::uuid, 'SALES_OUT', 'SO23-OUT-0001',
        'd0237000-0000-4000-8000-000000000004'::uuid, '收入确认');

-- ---------- 9. 凭证（期初 / 两张成本结转 / 收入 / 收款，全部借贷平衡） ----------
INSERT INTO biz_voucher (id, voucher_no, period, voucher_date, voucher_type, source_type,
                         source_id, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red)
VALUES
  ('d0237000-0000-4000-8000-000000000001'::uuid, 'SO23-V-OPEN', '2026-11', DATE '2026-11-01',
   'AUTO', 'MANUAL', NULL, 'SO23-OPEN', '期初存货建账', 6000.0000, 6000.0000, 2, 'POSTED', 0),
  ('d0237000-0000-4000-8000-000000000002'::uuid, 'SO23-V-COST-1', '2026-11', DATE '2026-11-05',
   'AUTO', 'STOCK_OUT', 'd0233000-0000-4000-8000-000000000001'::uuid, 'SO23-OUT-0001',
   '销售出库成本结转', 1000.0000, 1000.0000, 2, 'POSTED', 0),
  ('d0237000-0000-4000-8000-000000000003'::uuid, 'SO23-V-COST-2', '2026-11', DATE '2026-11-05',
   'AUTO', 'STOCK_OUT', 'd0233000-0000-4000-8000-000000000002'::uuid, 'SO23-OUT-0002',
   '销售出库成本结转', 1000.0000, 1000.0000, 2, 'POSTED', 0),
  ('d0237000-0000-4000-8000-000000000004'::uuid, 'SO23-V-INC-1', '2026-11', DATE '2026-11-06',
   'AUTO', 'INVOICE', 'd0236000-0000-4000-8000-000000000001'::uuid, 'SO23-IV-0001',
   '销售收入确认', 1130.0000, 1130.0000, 3, 'POSTED', 0),
  ('d0237000-0000-4000-8000-000000000005'::uuid, 'SO23-V-RCPT-1', '2026-11', DATE '2026-11-06',
   'AUTO', 'RECEIPT', 'd0235000-0000-4000-8000-000000000001'::uuid, 'SO23-RC-0001',
   '收到货款', 565.0000, 565.0000, 2, 'POSTED', 0);

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
VALUES
  ('d0237000-0000-4000-8000-000000000001'::uuid, 1, '期初存货', 'd1370004-0000-4000-8000-000000000001'::uuid,
   '1405', '库存商品', 6000.0000, 0),
  ('d0237000-0000-4000-8000-000000000001'::uuid, 2, '期初存货', 'a80732ac-dfec-42b7-95fd-c5eec0172684'::uuid,
   '1002', '银行存款', 0, 6000.0000),
  ('d0237000-0000-4000-8000-000000000002'::uuid, 1, '销售出库成本', 'd1370004-0000-4000-8000-000000000902'::uuid,
   '6401', '主营业务成本', 1000.0000, 0),
  ('d0237000-0000-4000-8000-000000000002'::uuid, 2, '销售出库成本', 'd1370004-0000-4000-8000-000000000001'::uuid,
   '1405', '库存商品', 0, 1000.0000),
  ('d0237000-0000-4000-8000-000000000003'::uuid, 1, '销售出库成本', 'd1370004-0000-4000-8000-000000000902'::uuid,
   '6401', '主营业务成本', 1000.0000, 0),
  ('d0237000-0000-4000-8000-000000000003'::uuid, 2, '销售出库成本', 'd1370004-0000-4000-8000-000000000001'::uuid,
   '1405', '库存商品', 0, 1000.0000),
  ('d0237000-0000-4000-8000-000000000004'::uuid, 1, '销售收入', '34353734-6952-4174-b061-7209c59091ac'::uuid,
   '1122', '应收账款', 1130.0000, 0),
  ('d0237000-0000-4000-8000-000000000004'::uuid, 2, '销售收入', 'e34438c3-20bc-4568-93e8-589a3ee7757a'::uuid,
   '5001', '主营业务收入', 0, 1000.0000),
  ('d0237000-0000-4000-8000-000000000004'::uuid, 3, '销项税', 'ff402e1b-b9a9-409e-a758-9b80ebc93eee'::uuid,
   '2221.02', '应交税费-销项税', 0, 130.0000),
  ('d0237000-0000-4000-8000-000000000005'::uuid, 1, '收到货款', 'a80732ac-dfec-42b7-95fd-c5eec0172684'::uuid,
   '1002', '银行存款', 565.0000, 0),
  ('d0237000-0000-4000-8000-000000000005'::uuid, 2, '收到货款', '34353734-6952-4174-b061-7209c59091ac'::uuid,
   '1122', '应收账款', 0, 565.0000);

COMMIT;

-- ---------- 10. 自检：夹具本身必须先把「该有的行」造齐（造漏了不是不变量红了，是夹具坏了） ----------
DO $$
DECLARE n bigint;
BEGIN
    SELECT count(*) INTO n FROM biz_sales_order WHERE so_no LIKE 'SO23-SO-%';
    IF n <> 1 THEN RAISE EXCEPTION 'SO-23 夹具：销售订单应有 1 张，实际 %', n; END IF;
    SELECT count(*) INTO n FROM biz_sales_order_item WHERE so_id = 'd0231000-0000-4000-8000-000000000001';
    IF n <> 2 THEN RAISE EXCEPTION 'SO-23 夹具：订单明细应有 2 行，实际 %', n; END IF;
    SELECT count(*) INTO n FROM biz_stock_out WHERE source_type = 'SALES';
    IF n <> 2 THEN RAISE EXCEPTION 'SO-23 夹具：SALES 出库单应有 2 张，实际 %', n; END IF;
    SELECT count(*) INTO n FROM biz_receivable WHERE source_type = 'SALES_OUT';
    IF n <> 2 THEN RAISE EXCEPTION 'SO-23 夹具：SALES_OUT 应收应有 2 张，实际 %', n; END IF;
    SELECT count(*) INTO n FROM biz_voucher WHERE voucher_no LIKE 'SO23-V-%';
    IF n <> 5 THEN RAISE EXCEPTION 'SO-23 夹具：凭证应有 5 张，实际 %', n; END IF;
    SELECT count(*) INTO n FROM biz_voucher_entry WHERE voucher_id IN
        (SELECT id FROM biz_voucher WHERE voucher_no LIKE 'SO23-V-%');
    IF n <> 11 THEN RAISE EXCEPTION 'SO-23 夹具：分录应有 11 条，实际 %', n; END IF;
    RAISE NOTICE 'SO-23 正向夹具就位：1 单 / 2 行 / 2 发货 / 2 出库 / 2 应收 / 1 收款 / 1 发票 / 5 凭证 / 11 分录';
END $$;
