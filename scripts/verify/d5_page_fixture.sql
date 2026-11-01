-- D5 页面验收夹具：三张不同状态的应付单（供 d5_page_check.mjs 断言按钮显隐）
-- ⚠️ 只在隔离库（cloudpivot_d5）上跑，绝不进生产。
delete from biz_status_log where entity_id in (select id from biz_payable where payable_no like 'AP-D5PG-%');
delete from biz_payment where payable_id in (select id from biz_payable where payable_no like 'AP-D5PG-%');
delete from biz_payable where payable_no like 'AP-D5PG-%';

insert into biz_supplier (id, supplier_code, supplier_name, status, credit_days)
values ('d5d50002-0000-4000-8000-000000000002'::uuid, 'D5PG-SUP', 'D5 页面验收供应商', 1, 30)
on conflict do nothing;

-- OPEN：应付 226、已付 0 → 按钮显示「未付 226.00」
insert into biz_payable (payable_no, supplier_id, source_type, source_id, source_no, currency_code,
                         amount, paid_amount, unpaid_amount, credit_days, due_date, period_code, status)
values ('AP-D5PG-OPEN', 'd5d50002-0000-4000-8000-000000000002'::uuid, 'STOCK_IN', gen_random_uuid(),
        'IN-D5PG-OPEN', 'CNY', 226.0000, 0, 226.0000, 30, date '2026-11-30', '2026-10', 'OPEN');

-- PARTIAL_PAID：应付 226、已付 100 → 按钮显示「未付 126.00」
insert into biz_payable (payable_no, supplier_id, source_type, source_id, source_no, currency_code,
                         amount, paid_amount, unpaid_amount, credit_days, due_date, period_code, status)
values ('AP-D5PG-PARTIAL', 'd5d50002-0000-4000-8000-000000000002'::uuid, 'STOCK_IN', gen_random_uuid(),
        'IN-D5PG-PARTIAL', 'CNY', 226.0000, 100.0000, 126.0000, 30, date '2026-11-30', '2026-10', 'PARTIAL_PAID');

-- SETTLED：应付 226、已付 226 → 按钮消失、说明「已结清」
insert into biz_payable (payable_no, supplier_id, source_type, source_id, source_no, currency_code,
                         amount, paid_amount, unpaid_amount, credit_days, due_date, period_code, status)
values ('AP-D5PG-SETTLED', 'd5d50002-0000-4000-8000-000000000002'::uuid, 'STOCK_IN', gen_random_uuid(),
        'IN-D5PG-SETTLED', 'CNY', 226.0000, 226.0000, 0, 30, date '2026-11-30', '2026-10', 'SETTLED');

select payable_no, status, amount, paid_amount, unpaid_amount
  from biz_payable where payable_no like 'AP-D5PG-%' order by payable_no;
