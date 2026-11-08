-- P3-07 后端小补真机验收夹具（隔离库 cloudpivot_p307be，一次性）
-- 只造「供应商 + 两张应付单」：付款 / 审批 / 执行全部走真实接口。
INSERT INTO biz_supplier (id, supplier_code, supplier_name, short_name, credit_days, status)
VALUES ('abe70001-0000-4000-8000-000000000001', 'SUP-BE1', 'P307BE 验收供应商', 'BE1', 30, 1)
ON CONFLICT (id) DO NOTHING;

-- AP-1：开关打开时用（审批通过 → 付款停在审批中 → awaiting_executor=true → 他人执行）
INSERT INTO biz_payable (
    id, payable_no, supplier_id, source_type, source_id, source_no,
    currency_code, amount, paid_amount, unpaid_amount, credit_days,
    due_date, period_code, status, remark)
VALUES (
    'abe70001-0000-4000-8000-000000000101', 'AP-BE1-0001',
    'abe70001-0000-4000-8000-000000000001', 'STOCK_IN',
    'abe70001-0000-4000-8000-000000000201', 'IN-BE1-0001',
    'CNY', 1200.0000, 0.0000, 1200.0000, 30,
    DATE '2026-11-30', '2026-11', 'OPEN', 'P307 后端验收夹具（开关打开）')
ON CONFLICT (id) DO NOTHING;

-- AP-2：开关关闭回归（审批通过 → 立即自动记账，付款直接 APPROVED）
INSERT INTO biz_payable (
    id, payable_no, supplier_id, source_type, source_id, source_no,
    currency_code, amount, paid_amount, unpaid_amount, credit_days,
    due_date, period_code, status, remark)
VALUES (
    'abe70001-0000-4000-8000-000000000102', 'AP-BE1-0002',
    'abe70001-0000-4000-8000-000000000001', 'STOCK_IN',
    'abe70001-0000-4000-8000-000000000202', 'IN-BE1-0002',
    'CNY', 800.0000, 0.0000, 800.0000, 30,
    DATE '2026-11-30', '2026-11', 'OPEN', 'P307 后端验收夹具（开关关闭回归）')
ON CONFLICT (id) DO NOTHING;

SELECT payable_no, amount, unpaid_amount, status, period_code
  FROM biz_payable WHERE payable_no LIKE 'AP-BE1-%' ORDER BY payable_no;
