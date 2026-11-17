-- M3-14 前端信用面板验收夹具（只落 145 的 cp_m314_page 演练库）
DELETE FROM biz_credit_log WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M314-%');
DELETE FROM biz_receivable WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M314-%');
DELETE FROM biz_sales_order WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M314-%');
DELETE FROM biz_customer WHERE code LIKE 'M314-%';

-- ① 正常客户：额度 100,000 / 已占 18,080 / 在审 7,540 / 可用 74,380（占用率 25.62%）
INSERT INTO biz_customer (id, code, name, credit_limit, credit_used, credit_policy, status)
VALUES ('c3140001-0000-4000-8000-000000000001', 'M314-PAGE-01', 'M314 正常客户（额度 10 万）', 100000, 18080, 'BLOCK', 'ACTIVE');
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax)
VALUES ('c3140002-0000-4000-8000-000000000001', 'SO-M314-0001', 'c3140001-0000-4000-8000-000000000001', CURRENT_DATE, 'APPROVED', 18080, 0, 18080);
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax)
VALUES ('c3140002-0000-4000-8000-000000000002', 'SO-M314-0002', 'c3140001-0000-4000-8000-000000000001', CURRENT_DATE, 'SUBMITTED', 7540, 0, 7540);
INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, amount,
                            received_amount, unsettled_amount, credit_days, period_code, due_date, status)
VALUES ('c3140003-0000-4000-8000-000000000001', 'AR-M314-0001', 'c3140001-0000-4000-8000-000000000001',
        'MANUAL', gen_random_uuid(), 'MANUAL', 18080, 0, 18080, 30, '2026-11', CURRENT_DATE + 30, 'OPEN');

-- ② 超额客户：额度 50,000 / 已占 60,000（超 10,000，BLOCK）；缓存列故意留 0 → 面板要报漂移
INSERT INTO biz_customer (id, code, name, credit_limit, credit_used, credit_policy, status)
VALUES ('c3140001-0000-4000-8000-000000000002', 'M314-PAGE-02', 'M314 超额客户（额度 5 万）', 50000, 0, 'BLOCK', 'ACTIVE');
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax)
VALUES ('c3140002-0000-4000-8000-000000000011', 'SO-M314-0011', 'c3140001-0000-4000-8000-000000000002', CURRENT_DATE, 'APPROVED', 60000, 0, 60000);

-- ③ 不控额度客户：额度 0（可用额度是 null，不是 0.00）
INSERT INTO biz_customer (id, code, name, credit_limit, credit_used, credit_policy, status)
VALUES ('c3140001-0000-4000-8000-000000000003', 'M314-PAGE-03', 'M314 不控额度客户（额度 0）', 0, 5000, 'WARN', 'ACTIVE');
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax)
VALUES ('c3140002-0000-4000-8000-000000000021', 'SO-M314-0021', 'c3140001-0000-4000-8000-000000000003', CURRENT_DATE, 'APPROVED', 5000, 0, 5000);

-- ④ 冻结客户：额度充足但冻结（比额度更硬的一道闸）
INSERT INTO biz_customer (id, code, name, credit_limit, credit_used, credit_policy, status)
VALUES ('c3140001-0000-4000-8000-000000000004', 'M314-PAGE-04', 'M314 冻结客户', 200000, 0, 'BLOCK', 'FROZEN');

SELECT code, credit_limit, credit_used, credit_policy, status FROM biz_customer WHERE code LIKE 'M314-%' ORDER BY code;
