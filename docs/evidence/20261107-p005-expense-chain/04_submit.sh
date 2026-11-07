#!/usr/bin/env bash
# ④ 提交审批：status=SUBMITTED + biz_flow_instance + 审批待办（biz_flow_task）
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
EMP=$(cat "$(tokf emp_drill)")
LEAD=$(cat "$(tokf lead_drill)")
RPT=$(cat "$EV/raw/report.id")

echo "===== ④-1 提交报销单（报销人 emp_drill）====="
api POST "/business/erp/expense/$RPT/submit" "$EMP" > "$EV/raw/r2_submit.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/r2_submit.json")"
jq -c '{report_no:.data.report.report_no,status:.data.report.status,approval_instance_id:.data.report.approval_instance_id,over_budget:.data.report.over_budget,over_amount:.data.report.over_amount,budget_usages:(.data.budget_usages|length),tier_label:.data.tier_label}' "$EV/raw/r2_submit.json"

echo "===== ④-2 DB 复核：单据状态 + 流程实例 + 待办任务 ====="
cat > /tmp/p005_submit_verify.sql <<SQL
\pset pager off
select report_no, status, submitted_at, approval_instance_id, over_budget, over_amount, budget_mode
  from biz_expense_report where id = '$RPT'::uuid;
\echo --- 流程实例 ---
select instance_no, biz_type, biz_no, status, title, initiator_name, started_at
  from biz_flow_instance where biz_id = '$RPT'::uuid order by created_at desc;
\echo --- 审批待办 biz_flow_task（真实待办表，节点名从 biz_flow_node 取）---
select t.id, t.node_key, n.node_name, t.assignee_name, t.assignee_source, t.status, t.round
  from biz_flow_task t
  join biz_flow_instance i on i.id = t.instance_id
  left join biz_flow_node n on n.id = t.instance_node_id
 where i.biz_id = '$RPT'::uuid order by t.created_at, t.node_key;
\echo --- 对照：biz_task（任务/项目模块的表，与审批待办不是一张表）---
select count(*) as biz_task_rows_for_this_report from biz_task where title like '%EXP-20261107%';
\echo --- 预算占用行 ---
select u.source_no, u.dept_id, u.account_id, u.period_code, u.amount, u.amount_net, u.status, u.over_budget, u.budget_id
  from biz_budget_usage u where u.source_id = '$RPT'::uuid;
SQL
psqlf /tmp/p005_submit_verify.sql

echo "===== ④-3 直属上级 lead_drill 的待办（HTTP 层）====="
api GET "/business/flow/task/todo?page=1&page_size=20" "$LEAD" > "$EV/raw/r2_todo_lead.json"
echo "http=$HTTP"
jq -c '{total:.data.total, rows:[.data.list[]?|{id,title,biz_no,biz_type,node_name}]}' "$EV/raw/r2_todo_lead.json"
