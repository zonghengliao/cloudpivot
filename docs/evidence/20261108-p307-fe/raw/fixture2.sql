-- AP-3：409 + 66013 用例（审批人本人点「执行付款」→ 内控拦）
INSERT INTO biz_payable (
    id, payable_no, supplier_id, source_type, source_id, source_no,
    currency_code, amount, paid_amount, unpaid_amount, credit_days,
    due_date, period_code, status, remark)
VALUES (
    'a3070000-0000-4000-8000-000000000103', 'AP-P307-0003',
    'a3070000-0000-4000-8000-000000000001', 'STOCK_IN',
    'a3070000-0000-4000-8000-000000000203', 'IN-P307-0003',
    'CNY', 600.0000, 0.0000, 600.0000, 30,
    DATE '2026-11-30', '2026-11', 'OPEN', 'P307 前端验收夹具（审批人自己执行 → 66013）')
ON CONFLICT (id) DO NOTHING;

SELECT payable_no, amount, status FROM biz_payable WHERE payable_no LIKE 'AP-P307-%' ORDER BY payable_no;
