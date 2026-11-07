\pset pager off
select table_name, string_agg(column_name, ', ' order by ordinal_position) as cols
  from information_schema.columns
 where table_name in ('biz_voucher_entry','biz_status_log','biz_expense_item','biz_budget_period')
 group by table_name;
