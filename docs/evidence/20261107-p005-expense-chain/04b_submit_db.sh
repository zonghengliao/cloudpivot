#!/usr/bin/env bash
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
RPT=$(cat "$EV/raw/report.id")
cat > /tmp/p005_submit_verify.sql <<SQL
\pset pager off
select report_no, status, approval_instance_id, over_budget, over_amount, budget_mode
  from biz_expense_report where id = '$RPT'::uuid;
\echo --- 流程实例 ---
select instance_no, biz_type, biz_no, status, title, initiator_name
  from biz_flow_instance where biz_id = '$RPT'::uuid order by created_at desc;
\echo --- 审批待办 biz_flow_task ---
select t.id, t.node_key, n.node_name, t.assignee_name, t.assignee_source, t.status, t.round
  from biz_flow_task t
  join biz_flow_instance i on i.id = t.instance_id
  left join biz_flow_node n on n.id = t.instance_node_id
 where i.biz_id = '$RPT'::uuid order by t.created_at, t.node_key;
\echo --- 对照 biz_task ---
select count(*) as biz_task_rows from biz_task where title like '%EXP-20261107%';
\echo --- 预算占用 ---
select source_no, period_code, amount, status, over_budget, budget_id from biz_budget_usage where source_id = '$RPT'::uuid;
SQL
psqlf /tmp/p005_submit_verify.sql
