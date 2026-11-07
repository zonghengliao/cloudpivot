-- ============================================================================
-- SO-44 验收夹具：销售目标达成率看板
--
-- 口径与期望值（期间 2026-11，另一口径验证 2026-Q4）：
--
--   销售员 a（销售一部）  订单 SO-SO44-0001 含税 11300（不含税 10000 + 税 1300）
--   订单明细            原料A 6000 + 780 = 6780 / 原料B 4000 + 520 = 4520（合计 11300）
--   出库 → 应收 11300 → 收款 5000（POSTED）→ 退款 1000（POSTED）
--
--   订单口径实际：SALESPERSON = 11300 / DEPT = 11300 / MATERIAL_CATEGORY(原料A) = 6780
--                 MATERIAL_CATEGORY(原料B) = 4520 / ALL = 11300
--   收款口径实际：SALESPERSON = 4000（5000 − 1000）/ DEPT = 4000 / ALL = 4000
--                 MATERIAL_CATEGORY：按含税占比分摊 → 原料A 4000×6780/11300 = 2400.00
--                                                     原料B 4000×4520/11300 = 1600.00
--
--   干扰数据（月度口径必须**不计**）：CANCELLED 订单 9999（同月）、2026-10 订单 8888（月外）。
--   注意 8888 只对**月度**期间是干扰数据：季报 2026-Q4 覆盖 10 月，季度口径应含它
--   ⇒ Q4 订单口径 = 11300 + 8888 = 20188。
--
-- 可重复执行：先按固定 uuid / 单号删自己写过的一切。
-- ============================================================================
\set ON_ERROR_STOP on

-- ---------- 0) 清场（目标 / 预测由 e2e 通过 API 建，这里清同名期间） ----------
DELETE FROM biz_sales_target   WHERE period_code IN ('2026-11', '2026-Q4');
DELETE FROM biz_sales_forecast WHERE period_code IN ('2026-11', '2026-Q4');

DELETE FROM biz_receipt_refund_allocation WHERE refund_id = 'aa440000-0000-4000-8000-000000000012'::uuid;
DELETE FROM biz_receipt_refund            WHERE id      = 'aa440000-0000-4000-8000-000000000012'::uuid;
DELETE FROM biz_receipt_allocation        WHERE receipt_id = 'aa440000-0000-4000-8000-000000000011'::uuid;
DELETE FROM biz_receipt                   WHERE id      = 'aa440000-0000-4000-8000-000000000011'::uuid;
DELETE FROM biz_receivable                WHERE ar_no LIKE 'AR-SO44-%';
DELETE FROM biz_stock_out                 WHERE out_no LIKE 'OUT-SO44-%';
DELETE FROM biz_shipment                  WHERE ship_no LIKE 'SH-SO44-%';
DELETE FROM biz_sales_order               WHERE so_no LIKE 'SO-SO44-%';

-- ---------- 1) 部门 + 销售员 ----------
INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, status, sort_order)
VALUES ('aa440000-0000-4000-8000-000000000001'::uuid, 'D-SO44', '销售一部', NULL, 1, 10)
ON CONFLICT (id) DO UPDATE SET dept_name = EXCLUDED.dept_name, deleted_at = NULL;

INSERT INTO sys_user (id, username, password, real_name, dept_id, status)
SELECT 'aa440000-0000-4000-8000-000000000002'::uuid, 'so44_sales_a',
       (SELECT password FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL),
       '销售员甲', 'aa440000-0000-4000-8000-000000000001'::uuid, 1
ON CONFLICT (id) DO UPDATE
   SET dept_id = EXCLUDED.dept_id, real_name = EXCLUDED.real_name, deleted_at = NULL;

-- ---------- 2) 销售订单（含税 11300：原料A 6780 + 原料B 4520） ----------
INSERT INTO biz_sales_order
    (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax,
     salesperson_id, remark)
VALUES
    ('aa440000-0000-4000-8000-000000000003'::uuid, 'SO-SO44-0001',
     'd0260000-0000-4000-8000-000000000001'::uuid, DATE '2026-11-05', 'APPROVED',
     10000.00, 1300.00, 11300.00, 'aa440000-0000-4000-8000-000000000002'::uuid, 'SO-44 夹具');

INSERT INTO biz_sales_order_item
    (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount)
VALUES
    ('aa440000-0000-4000-8000-000000000031'::uuid, 'aa440000-0000-4000-8000-000000000003'::uuid,
     1, 'd1370003-0000-4000-8000-000000000001'::uuid, 60, 100.000000, 6000.00, 13, 780.00),
    ('aa440000-0000-4000-8000-000000000032'::uuid, 'aa440000-0000-4000-8000-000000000003'::uuid,
     2, 'd1370003-0000-4000-8000-000000000002'::uuid, 40, 100.000000, 4000.00, 13, 520.00);

-- 干扰订单：作废（不计）+ 期外 2026-10（不计）
INSERT INTO biz_sales_order
    (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax, salesperson_id, remark)
VALUES
    ('aa440000-0000-4000-8000-000000000004'::uuid, 'SO-SO44-0002',
     'd0260000-0000-4000-8000-000000000001'::uuid, DATE '2026-11-06', 'CANCELLED',
     8848.00, 1151.00, 9999.00, 'aa440000-0000-4000-8000-000000000002'::uuid, '作废单，不计业绩'),
    ('aa440000-0000-4000-8000-000000000005'::uuid, 'SO-SO44-0003',
     'd0260000-0000-4000-8000-000000000001'::uuid, DATE '2026-10-20', 'OUTBOUND',
     7865.49, 1022.51, 8888.00, 'aa440000-0000-4000-8000-000000000002'::uuid, '十月单，十一月口径不计');

-- ---------- 3) 发货 → 出库 → 应收（收款口径的归属链） ----------
INSERT INTO biz_shipment
    (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty)
VALUES
    ('aa440000-0000-4000-8000-000000000006'::uuid, 'SH-SO44-0001',
     'aa440000-0000-4000-8000-000000000003'::uuid,
     'd0260000-0000-4000-8000-000000000001'::uuid,
     'd1370001-0000-4000-8000-000000000001'::uuid, DATE '2026-11-06', 'POSTED', 100);

INSERT INTO biz_stock_out
    (id, out_no, out_type, warehouse_id, out_date, status, total_qty, total_amount,
     source_type, source_id)
VALUES
    ('aa440000-0000-4000-8000-000000000007'::uuid, 'OUT-SO44-0001', 'OTHER',
     'd1370001-0000-4000-8000-000000000001'::uuid, DATE '2026-11-06', 'POSTED', 100, 6000.00,
     'SALES', 'aa440000-0000-4000-8000-000000000006'::uuid);

UPDATE biz_shipment SET stock_out_id = 'aa440000-0000-4000-8000-000000000007'::uuid
 WHERE id = 'aa440000-0000-4000-8000-000000000006'::uuid;

INSERT INTO biz_receivable
    (id, ar_no, customer_id, source_type, source_id, source_no, amount, received_amount,
     unsettled_amount, credit_days, due_date, period_code, status)
VALUES
    ('aa440000-0000-4000-8000-000000000010'::uuid, 'AR-SO44-0001',
     'd0260000-0000-4000-8000-000000000001'::uuid, 'SALES_OUT',
     'aa440000-0000-4000-8000-000000000007'::uuid, 'OUT-SO44-0001',
     11300.00, 4000.00, 7300.00, 30, DATE '2026-12-06', '2026-11', 'PARTIAL');

-- ---------- 4) 收款 5000（POSTED）→ 核销；退款 1000（POSTED）→ 冲减 ----------
INSERT INTO biz_receipt
    (id, receipt_no, customer_id, receipt_date, amount, allocated_amount, method, status)
VALUES
    ('aa440000-0000-4000-8000-000000000011'::uuid, 'RC-SO44-0001',
     'd0260000-0000-4000-8000-000000000001'::uuid, DATE '2026-11-10',
     5000.00, 5000.00, 'BANK_TRANSFER', 'POSTED');

INSERT INTO biz_receipt_allocation (id, receipt_id, receivable_id, amount, remark)
VALUES
    ('aa440000-0000-4000-8000-000000000021'::uuid, 'aa440000-0000-4000-8000-000000000011'::uuid,
     'aa440000-0000-4000-8000-000000000010'::uuid, 5000.00, 'SO-44 夹具');

INSERT INTO biz_receipt_refund
    (id, refund_no, customer_id, source_receipt_id, refund_date, amount, allocated_amount,
     method, status, reason)
VALUES
    ('aa440000-0000-4000-8000-000000000012'::uuid, 'RR-SO44-0001',
     'd0260000-0000-4000-8000-000000000001'::uuid, 'aa440000-0000-4000-8000-000000000011'::uuid,
     DATE '2026-11-20', 1000.00, 1000.00, 'BANK_TRANSFER', 'POSTED', '客户部分退货退款');

INSERT INTO biz_receipt_refund_allocation (id, refund_id, receivable_id, amount, remark)
VALUES
    ('aa440000-0000-4000-8000-000000000022'::uuid, 'aa440000-0000-4000-8000-000000000012'::uuid,
     'aa440000-0000-4000-8000-000000000010'::uuid, 1000.00, 'SO-44 夹具');

-- ---------- 5) 落库核对（人眼可查） ----------
\echo '--- 订单（2026-11 有效）---'
SELECT so_no, status, order_date, total_with_tax, salesperson_id FROM biz_sales_order
 WHERE so_no LIKE 'SO-SO44-%' ORDER BY so_no;
\echo '--- 收款 / 退款 ---'
SELECT receipt_no, status, receipt_date, amount FROM biz_receipt WHERE receipt_no LIKE 'RC-SO44-%'
UNION ALL
SELECT refund_no, status, refund_date, amount FROM biz_receipt_refund WHERE refund_no LIKE 'RR-SO44-%';
