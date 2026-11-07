\pset pager off
\set dept '''01a1b602-8e43-7436-93a2-482d710f21d8'''

\echo ===================== 1. 预算版本链（本卡造的 DRILL_BDG 部门）=====================
select budget_no, fiscal_year, account_code, version_no, change_type, status, control_mode,
       total_amount, effective_from, retired_at is not null as retired
  from biz_budget
 where dept_id = '01a1b602-8e43-7436-93a2-482d710f21d8'::uuid
 order by account_code, version_no;

\echo ===================== 2. 占用台账（逐槽）=====================
select b.account_code, u.period_code, u.source_no, u.amount, u.status, u.over_budget, u.over_amount,
       (u.budget_id is not null) as has_budget_id
  from biz_budget_usage u
  join biz_budget b on b.id = u.budget_id
 where u.dept_id = '01a1b602-8e43-7436-93a2-482d710f21d8'::uuid
 union all
select '-' as account_code, u.period_code, u.source_no, u.amount, u.status, u.over_budget, u.over_amount, false
  from biz_budget_usage u
 where u.dept_id = '01a1b602-8e43-7436-93a2-482d710f21d8'::uuid and u.budget_id is null
 order by 1, 3;

\echo ===================== 3. 演练单据（EXP-000009..000014）=====================
select report_no, status, total_with_tax, over_budget, over_amount, budget_mode,
       approval_instance_id is not null as has_inst
  from biz_expense_report
 where report_no between 'EXP-20261107-000009' and 'EXP-20261107-000014'
 order by report_no;

\echo ===================== 4. 单据总额 = 明细合计 =====================
select r.report_no, r.total_with_tax,
       coalesce(sum(i.with_tax_amount), 0) as items_sum,
       count(i.id) as item_cnt,
       (r.total_with_tax = coalesce(sum(i.with_tax_amount), 0)) as header_eq_items
  from biz_expense_report r
  left join biz_expense_item i on i.report_id = r.id
 where r.report_no between 'EXP-20261107-000009' and 'EXP-20261107-000014'
 group by r.report_no, r.total_with_tax
 order by r.report_no;

\echo ===================== 5. 本次演练的凭证借贷平衡 =====================
select v.voucher_no, v.source_type, v.source_no, v.entry_count, v.total_debit, v.total_credit,
       (v.total_debit = v.total_credit) as balanced, v.status
  from biz_voucher v
 where v.source_id in (select id from biz_expense_report
                        where report_no between 'EXP-20261107-000009' and 'EXP-20261107-000014')
 order by v.voucher_no;

\echo ===================== 6. 凭证分录（借/贷明细）=====================
select v.voucher_no, l.summary, a.account_code, a.account_name, l.debit_amount, l.credit_amount
  from biz_voucher v
  join biz_voucher_entry l on l.voucher_id = v.id
  join biz_account a on a.id = l.account_id
 where v.source_id in (select id from biz_expense_report
                        where report_no between 'EXP-20261107-000009' and 'EXP-20261107-000014')
 order by v.voucher_no, l.line_no;

\echo ===================== 7. 预算执行（报表同源 SQL：ACTIVE × 槽占用）=====================
select b.account_code, b.total_amount,
       coalesce(round(sum(u.amount) filter (where u.status = 'OCCUPIED'), 4), 0) as occupied,
       coalesce(round(sum(u.amount) filter (where u.status = 'CONSUMED'), 4), 0) as consumed,
       coalesce(round(sum(u.amount) filter (where u.status in ('OCCUPIED','CONSUMED')), 4), 0) as used,
       b.total_amount - coalesce(round(sum(u.amount) filter (where u.status in ('OCCUPIED','CONSUMED')), 4), 0) as available,
       case when b.total_amount > 0
            then round(coalesce(sum(u.amount) filter (where u.status in ('OCCUPIED','CONSUMED')), 0) / b.total_amount * 100, 2)
            else 0 end as exec_ratio
  from biz_budget b
  left join biz_budget_usage u
    on u.dept_id = b.dept_id and u.account_id = b.account_id and u.fiscal_year = b.fiscal_year
   and u.deleted_at is null
 where b.status = 'ACTIVE' and b.fiscal_year = 2026
   and b.dept_id = '01a1b602-8e43-7436-93a2-482d710f21d8'::uuid
 group by b.account_code, b.total_amount
 order by b.account_code;

\echo ===================== 8. 状态机审计日志（预算/报销单的迁移痕迹）=====================
select entity_type, entity_no, from_status, to_status,
       operator_id is not null as has_operator_id, nullif(operator_name, '') is not null as has_operator_name
  from biz_status_log
 where entity_type in ('BUDGET','EXPENSE_REPORT')
   and entity_no between 'BDG-20261107-000002' and 'BDG-20261107-000005'
    or entity_no between 'EXP-20261107-000009' and 'EXP-20261107-000014'
 order by created_at;
