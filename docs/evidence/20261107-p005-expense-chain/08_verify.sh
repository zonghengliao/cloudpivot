#!/usr/bin/env bash
# ⑧ 收口核对：金额勾稽 / 凭证借贷平衡 / 科目归集 / 总账试算 / 预算台账 / 审批链
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
RPT=$(cat "$EV/raw/report.id")
DEPT=$(cat "$EV/raw/dept.id")
OUT="$EV/08_verify_output.txt"

cat > /tmp/p005_final_verify.sql <<SQL
\pset pager off
\echo ========== 1. 本次演练的全部报销单 ==========
select report_no, title, status, pay_status, total_amount, total_with_tax, item_count,
       over_budget, over_amount, budget_mode, approval_instance_id is not null as has_instance,
       voucher_id is not null as has_voucher
  from biz_expense_report
 where dept_id = '$DEPT'::uuid and deleted_at is null
 order by report_no;

\echo ========== 2. 金额勾稽：主表 = 明细合计（逐单） ==========
select r.report_no, r.total_with_tax as header_amount,
       coalesce(i.item_sum, 0) as item_sum, i.item_rows,
       (r.total_with_tax = coalesce(i.item_sum, 0)) as header_eq_items,
       r.item_count, (r.item_count = i.item_rows) as count_eq
  from biz_expense_report r
  left join (select report_id, sum(with_tax_amount) as item_sum, count(*) as item_rows
               from biz_expense_item group by report_id) i on i.report_id = r.id
 where r.dept_id = '$DEPT'::uuid and r.deleted_at is null
 order by r.report_no;

\echo ========== 3. 凭证：来源类型 / 借贷平衡（本次演练相关） ==========
select v.voucher_no, v.source_type, v.source_no, v.voucher_date, v.period, v.status,
       v.total_debit, v.total_credit, (v.total_debit = v.total_credit) as balanced,
       coalesce(e.dr, 0) as entry_debit, coalesce(e.cr, 0) as entry_credit,
       (v.total_debit = coalesce(e.dr,0) and v.total_credit = coalesce(e.cr,0)) as entries_match
  from biz_voucher v
  left join (select voucher_id, sum(debit_amount) as dr, sum(credit_amount) as cr
               from biz_voucher_entry group by voucher_id) e on e.voucher_id = v.id
 where v.source_no in (select report_no from biz_expense_report where dept_id = '$DEPT'::uuid)
    or v.source_no in (select p.payment_no from biz_expense_payment p
                        join biz_expense_report r on r.id = p.report_id where r.dept_id = '$DEPT'::uuid)
 order by v.voucher_no;

\echo ========== 4. 凭证分录（科目归集：是否落在报销类型关联的 5602.02） ==========
select v.voucher_no, v.source_type, e.line_no, e.account_code, e.account_name,
       e.debit_amount, e.credit_amount,
       case when e.account_id = t.account_id then 'yes' else '-' end as is_type_account
  from biz_voucher v
  join biz_voucher_entry e on e.voucher_id = v.id
  left join biz_expense_type t on t.type_code = 'DRILL_TRAVEL' and t.deleted_at is null
 where v.source_no in (select report_no from biz_expense_report where dept_id = '$DEPT'::uuid)
 order by v.voucher_no, e.line_no;

\echo ========== 5. 科目余额（本次演练凭证范围） ==========
select a.account_code, a.account_name,
       sum(e.debit_amount) as debit, sum(e.credit_amount) as credit,
       sum(e.debit_amount - e.credit_amount) as net_debit
  from biz_voucher_entry e join biz_account a on a.id = e.account_id
  join biz_voucher v on v.id = e.voucher_id
 where v.source_no in (select report_no from biz_expense_report where dept_id = '$DEPT'::uuid)
    or v.source_no in (select p.payment_no from biz_expense_payment p
                        join biz_expense_report r on r.id = p.report_id where r.dept_id = '$DEPT'::uuid)
 group by a.account_code, a.account_name order by a.account_code;

\echo ========== 6. 全账试算平衡（不只看本单，防串账） ==========
select count(*) as vouchers, sum(total_debit) as total_debit, sum(total_credit) as total_credit,
       (sum(total_debit) = sum(total_credit)) as balanced
  from biz_voucher where deleted_at is null and status = 'POSTED';

\echo ========== 7. 预算台账 ==========
select b.budget_no, b.status, b.control_mode, b.total_amount, b.effective_from, b.approval_instance_id
  from biz_budget b where b.dept_id = '$DEPT'::uuid and b.deleted_at is null;
select u.source_no, u.amount, u.status, u.budget_id is not null as has_budget, u.over_budget, u.release_reason
  from biz_budget_usage u
 where u.dept_id = '$DEPT'::uuid and u.account_id = '$ACC_TRAVEL'::uuid
 order by u.created_at;

\echo ========== 8. 审批链（实例 + 每条待办） ==========
select i.instance_no, i.biz_type, i.biz_no, i.status, i.initiator_name,
       (select count(*) from biz_flow_task t where t.instance_id = i.id) as task_cnt,
       (select count(*) from biz_flow_task t where t.instance_id = i.id and t.status = 'APPROVED') as approved_cnt
  from biz_flow_instance i
 where i.biz_no in (select report_no from biz_expense_report where dept_id = '$DEPT'::uuid)
    or i.biz_id in (select id from biz_budget where dept_id = '$DEPT'::uuid)
 order by i.created_at;
select i.biz_no, t.node_key, n.node_name, t.assignee_name, t.assignee_source, t.status, t.is_self_approval
  from biz_flow_task t
  join biz_flow_instance i on i.id = t.instance_id
  left join biz_flow_node n on n.version_id = i.version_id and n.node_key = t.node_key
 where i.biz_no in (select report_no from biz_expense_report where dept_id = '$DEPT'::uuid)
    or i.biz_id in (select id from biz_budget where dept_id = '$DEPT'::uuid)
 order by i.created_at, t.created_at;

\echo ========== 9. 采购/销售/库存不变量未被污染（本卡只新增数据） ==========
select count(*) as expense_reports_total from biz_expense_report where deleted_at is null;
select count(*) as vouchers_total from biz_voucher where deleted_at is null;
SQL
psqlf /tmp/p005_final_verify.sql 2>&1 | tee "$OUT"
echo
echo "===== 证据已写入 $OUT ====="
