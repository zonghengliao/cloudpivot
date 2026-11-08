-- P3-07 前端配套验收夹具（隔离库 cloudpivot_p307fe，一次性）
-- 只造「应付单 + 供应商」两行：付款 / 审批 / 执行全部走真实接口（本卡不测收货链路）。
-- 单号带 P307 前缀，收尾可整体删库。
INSERT INTO biz_supplier (id, supplier_code, supplier_name, short_name, credit_days, status)
VALUES ('a3070000-0000-4000-8000-000000000001', 'SUP-P307', 'P307 验收供应商', 'P307', 30, 1)
ON CONFLICT (id) DO NOTHING;

-- AP-1：开关打开后用（审批通过 → 付款停在审批中 → 前端「执行付款」）
INSERT INTO biz_payable (
    id, payable_no, supplier_id, source_type, source_id, source_no,
    currency_code, amount, paid_amount, unpaid_amount, credit_days,
    due_date, period_code, status, remark)
VALUES (
    'a3070000-0000-4000-8000-000000000101', 'AP-P307-0001',
    'a3070000-0000-4000-8000-000000000001', 'STOCK_IN',
    'a3070000-0000-4000-8000-000000000201', 'IN-P307-0001',
    'CNY', 1200.0000, 0.0000, 1200.0000, 30,
    DATE '2026-11-30', '2026-11', 'OPEN', 'P307 前端验收夹具（开关打开）')
ON CONFLICT (id) DO NOTHING;

-- AP-2：开关关闭后用（审批通过 → 立即自动记账，付款直接 APPROVED，按钮不该出现）
INSERT INTO biz_payable (
    id, payable_no, supplier_id, source_type, source_id, source_no,
    currency_code, amount, paid_amount, unpaid_amount, credit_days,
    due_date, period_code, status, remark)
VALUES (
    'a3070000-0000-4000-8000-000000000102', 'AP-P307-0002',
    'a3070000-0000-4000-8000-000000000001', 'STOCK_IN',
    'a3070000-0000-4000-8000-000000000202', 'IN-P307-0002',
    'CNY', 800.0000, 0.0000, 800.0000, 30,
    DATE '2026-11-30', '2026-11', 'OPEN', 'P307 前端验收夹具（开关关闭回归）')
ON CONFLICT (id) DO NOTHING;

SELECT payable_no, amount, unpaid_amount, status, period_code FROM biz_payable WHERE payable_no LIKE 'AP-P307-%' ORDER BY payable_no;
