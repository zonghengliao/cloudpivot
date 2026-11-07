#!/usr/bin/env bash
# ⑤ 走完整审批流：直属上级 lead_drill 通过 → 财务经理 fin_drill 通过 → APPROVED + 计提凭证
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
LEAD=$(cat "$(tokf lead_drill)")
FIN=$(cat "$(tokf fin_drill)")
RPT=$(cat "$EV/raw/report.id")

todoid() { # $1=token  → 取该报销单的第一条待办 id
  api GET "/business/flow/task/todo?page=1&page_size=50" "$1" > /tmp/_p005_todo.json
  jq -r --arg t "EXP" '[.data.list[]?|select((.title//"")|startswith($t))][0].id // empty' /tmp/_p005_todo.json
}

echo "===== ⑤-1 直属上级 lead_drill 审批通过 ====="
T1=$(todoid "$LEAD")
echo "todo(lead)=$T1"
[ -n "$T1" ] || { echo "FATAL: lead_drill 没有待办"; exit 1; }
api POST "/business/flow/task/$T1/approve" "$LEAD" '{"comment":"P0-05 演练：直属上级同意"}' > "$EV/raw/ap1_lead.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/ap1_lead.json")"
sleep 2

echo "===== ⑤-2 财务经理 fin_drill 审批通过 ====="
T2=$(todoid "$FIN")
echo "todo(fin)=$T2"
[ -n "$T2" ] || { echo "FATAL: fin_drill 没有待办（金额分级没走到财务经理？）"; exit 1; }
api POST "/business/flow/task/$T2/approve" "$FIN" '{"comment":"P0-05 演练：财务同意"}' > "$EV/raw/ap2_fin.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/ap2_fin.json")"
sleep 3

echo "===== ⑤-3 DB 复核：单据 / 实例 / 待办 / 计提凭证 ====="
cat > /tmp/p005_approve_verify.sql <<SQL
\pset pager off
select report_no, status, approved_at, approved_by, approval_instance_id, voucher_id, paid_amount, unpaid_amount, pay_status
  from biz_expense_report where id = '$RPT'::uuid;
\echo --- 流程实例状态 ---
select instance_no, status, ended_at, result_comment from biz_flow_instance where biz_id = '$RPT'::uuid order by created_at desc;
\echo --- 全部待办（应为两条 COMPLETED）---
select t.node_key, t.assignee_name, t.status, t.comment, t.completed_at
  from biz_flow_task t join biz_flow_instance i on i.id = t.instance_id
 where i.biz_id = '$RPT'::uuid order by t.created_at;
\echo --- 计提凭证 ---
select v.voucher_no, v.source_type, v.source_no, v.voucher_date, v.period, v.status,
       v.total_debit, v.total_credit, (v.total_debit = v.total_credit) as balanced
  from biz_voucher v where v.source_no = (select report_no from biz_expense_report where id = '$RPT'::uuid) order by v.voucher_no;
\echo --- 凭证分录 ---
select v.voucher_no, e.line_no, e.account_code, e.account_name, e.debit_amount, e.credit_amount, e.summary
  from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id
 where v.source_no = (select report_no from biz_expense_report where id = '$RPT'::uuid)
 order by v.voucher_no, e.line_no;
\echo --- 预算占用状态 ---
select source_no, amount, status, consumed_at, voucher_id from biz_budget_usage where source_id = '$RPT'::uuid;
SQL
psqlf /tmp/p005_approve_verify.sql
