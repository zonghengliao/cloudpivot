-- SO-43 验收夹具（隔离库 cloudpivot_so43）
-- 造一个客户的 5 类事件，覆盖：期初（9 月挂账）、挂账、退货冲减、坏账核销、收款、收款退款、
-- 以及一张「作废应收」用来验证整笔剔除。
--
-- 期望值（period 2026-10-01 ~ 2026-10-31，CNY）：
--   期初 = AR1 10000 + AR3 2000 = 12000
--   明细 = AR2 +5000 / RETURN -1000 / AR3 WRITE_OFF -2000 / RC1 -3000 / RR1 +500
--   期末 = 12000 + 5000 - 1000 - 2000 - 3000 + 500 = 11500
--   台账核对：AR1 未收 10000-2500=7500 + AR2 未收 4000 = 11500
BEGIN;

-- 干净起点（可重复执行）
DELETE FROM biz_receipt_refund_allocation WHERE refund_id IN (SELECT id FROM biz_receipt_refund WHERE refund_no LIKE 'RR-SO43-%');
DELETE FROM biz_receipt_refund WHERE refund_no LIKE 'RR-SO43-%';
DELETE FROM biz_receipt_allocation WHERE receipt_id IN (SELECT id FROM biz_receipt WHERE receipt_no LIKE 'RC-SO43-%');
DELETE FROM biz_receipt WHERE receipt_no LIKE 'RC-SO43-%';
DELETE FROM biz_sales_return WHERE return_no LIKE 'SR-SO43-%';
DELETE FROM biz_statement_confirm WHERE customer_id = 'aa430000-0000-4000-8000-000000000001'::uuid;
DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-SO43-%';
DELETE FROM biz_customer WHERE code = 'CUS-SO43';

-- 客户
INSERT INTO biz_customer (id, code, name, tax_no, credit_limit, credit_used, status, credit_days)
VALUES ('aa430000-0000-4000-8000-000000000001'::uuid, 'CUS-SO43', '联调客户（SO-43）',
        '91110000SO4300001', 0, 0, 'ACTIVE', 30);

-- AR1：9 月挂账 10000（期初），10 月收 3000 又退 500 ⇒ 未收 7500
INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, currency_code,
                            amount, received_amount, unsettled_amount, credit_days, due_date, period_code, status,
                            created_at, updated_at)
VALUES ('aa430000-0000-4000-8000-0000000000a1'::uuid, 'AR-SO43-0001',
        'aa430000-0000-4000-8000-000000000001'::uuid, 'MANUAL',
        'aa430000-0000-4000-8000-0000000000b1'::uuid, '手工挂账', 'CNY',
        10000, 2500, 7500, 30, '2026-10-10', '2026-09', 'PARTIAL',
        '2026-09-10 10:00:00+08', '2026-10-25 10:00:00+08');

-- AR2：10 月挂账 5000，10 月退 1000（退货过账原地扣减 ⇒ amount 4000）
INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, currency_code,
                            amount, received_amount, unsettled_amount, credit_days, due_date, period_code, status,
                            created_at, updated_at)
VALUES ('aa430000-0000-4000-8000-0000000000a2'::uuid, 'AR-SO43-0002',
        'aa430000-0000-4000-8000-000000000001'::uuid, 'SALES_OUT',
        'aa430000-0000-4000-8000-0000000000b2'::uuid, 'SO-SO43-0001', 'CNY',
        4000, 0, 4000, 30, '2026-11-04', '2026-10', 'OPEN',
        '2026-10-05 10:00:00+08', '2026-10-12 10:00:00+08');

-- AR3：9 月挂账 2000，10 月坏账核销（期初 2000，期末 0）
INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, currency_code,
                            amount, received_amount, unsettled_amount, credit_days, due_date, period_code, status,
                            settled_at, created_at, updated_at)
VALUES ('aa430000-0000-4000-8000-0000000000a3'::uuid, 'AR-SO43-0003',
        'aa430000-0000-4000-8000-000000000001'::uuid, 'MANUAL',
        'aa430000-0000-4000-8000-0000000000b3'::uuid, '手工挂账', 'CNY',
        2000, 0, 2000, 0, '2026-09-30', '2026-09', 'WRITTEN_OFF',
        '2026-10-18 09:00:00+08', '2026-09-01 10:00:00+08', '2026-10-18 09:00:00+08');

-- AR4：作废应收（出库作废）—— 金额不许进对账单
INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, currency_code,
                            amount, received_amount, unsettled_amount, credit_days, due_date, period_code, status,
                            created_at, updated_at)
VALUES ('aa430000-0000-4000-8000-0000000000a4'::uuid, 'AR-SO43-0004',
        'aa430000-0000-4000-8000-000000000001'::uuid, 'SALES_OUT',
        'aa430000-0000-4000-8000-0000000000b4'::uuid, 'SO-SO43-0004', 'CNY',
        999, 0, 999, 0, '2026-10-02', '2026-10', 'CANCELLED',
        '2026-10-02 10:00:00+08', '2026-10-02 11:00:00+08');

-- 退货：挂在 AR2 上，10-12 过账 1000（含税）
INSERT INTO biz_sales_return (id, return_no, customer_id, return_date, return_type, warehouse_id,
                              status, total_amount, total_tax, total_with_tax, receivable_id, created_at, updated_at)
VALUES ('aa430000-0000-4000-8000-0000000000c1'::uuid, 'SR-SO43-0001',
        'aa430000-0000-4000-8000-000000000001'::uuid, '2026-10-12', 'QUALITY',
        'aa430000-0000-4000-8000-0000000000d1'::uuid, 'POSTED', 884.96, 115.04, 1000,
        'aa430000-0000-4000-8000-0000000000a2'::uuid, '2026-10-12 10:00:00+08', '2026-10-12 10:00:00+08');

-- 收款：10-20 收 3000 全核销到 AR1
INSERT INTO biz_receipt (id, receipt_no, customer_id, receipt_date, amount, allocated_amount, method, status,
                         created_at, updated_at)
VALUES ('aa430000-0000-4000-8000-0000000000e1'::uuid, 'RC-SO43-0001',
        'aa430000-0000-4000-8000-000000000001'::uuid, '2026-10-20', 3000, 3000, 'BANK_TRANSFER', 'POSTED',
        '2026-10-20 10:00:00+08', '2026-10-20 10:00:00+08');
INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount, created_at)
VALUES ('aa430000-0000-4000-8000-0000000000e1'::uuid, 'aa430000-0000-4000-8000-0000000000a1'::uuid,
        3000, '2026-10-20 10:00:00+08');

-- 收款退款：10-25 退 500（冲回 AR1 已收）
INSERT INTO biz_receipt_refund (id, refund_no, customer_id, source_receipt_id, refund_date, amount,
                                allocated_amount, method, status, reason, created_at, updated_at)
VALUES ('aa430000-0000-4000-8000-0000000000f1'::uuid, 'RR-SO43-0001',
        'aa430000-0000-4000-8000-000000000001'::uuid, 'aa430000-0000-4000-8000-0000000000e1'::uuid,
        '2026-10-25', 500, 500, 'BANK_TRANSFER', 'POSTED', '客户多付退回（SO-43 联调）',
        '2026-10-25 10:00:00+08', '2026-10-25 10:00:00+08');
INSERT INTO biz_receipt_refund_allocation (refund_id, receivable_id, amount)
VALUES ('aa430000-0000-4000-8000-0000000000f1'::uuid, 'aa430000-0000-4000-8000-0000000000a1'::uuid, 500);

COMMIT;
