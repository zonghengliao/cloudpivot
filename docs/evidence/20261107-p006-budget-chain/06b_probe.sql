\pset pager off
\echo === 本次演练的占用行（budget_id 是否回填）===
select u.source_no, u.amount, u.status, u.budget_id, u.over_budget, u.period_code
  from biz_budget_usage u
 where u.source_no in ('EXP-20261107-000009','EXP-20261107-000010')
 order by u.source_no;
\echo === 凭证（列名先看）===
select column_name from information_schema.columns where table_name='biz_voucher' order by ordinal_position;
\echo === 本次两张报销单的凭证 ===
select v.voucher_no, v.source_type, v.source_id, v.total_debit, v.total_credit, v.status
  from biz_voucher v
 where v.source_type = 'EXPENSE_REPORT'
   and v.source_id in (select id from biz_expense_report where report_no in ('EXP-20261107-000009','EXP-20261107-000010'))
 order by v.voucher_no;
