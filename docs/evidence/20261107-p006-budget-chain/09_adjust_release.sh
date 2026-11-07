#!/usr/bin/env bash
# ⑥ 续：BLOCK 策略下「超支」的合法放行路径 = 调整预算（新版本走审批），然后重提被拦的单
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh

ADMIN=$(cat "$(tokf admin)")
EMP=$(cat "$(tokf emp_drill)")
DEPT=$(cat "$EV/raw/dept.id")
BUD=$(cat "$EV/raw/budget.id")
R1000=$(cat "$EV/raw/r1000.id")

echo "===== ⑥-5 调整预算 10000 → 11000（POST /budget/{id}/adjust，自动送审）====="
PERIODS='[{"month":1,"amount":"0"},{"month":2,"amount":"0"},{"month":3,"amount":"0"},{"month":4,"amount":"0"},{"month":5,"amount":"0"},{"month":6,"amount":"0"},{"month":7,"amount":"0"},{"month":8,"amount":"0"},{"month":9,"amount":"0"},{"month":10,"amount":"0"},{"month":11,"amount":"11000"},{"month":12,"amount":"0"}]'
BODY="{\"total_amount\":\"11000\",\"control_mode\":\"BLOCK\",\"periods\":$PERIODS,\"change_reason\":\"P0-06 演练：差旅费超支 500，按总经理批准追加 1000 额度\"}"
api POST "/business/erp/budget/$BUD/adjust" "$ADMIN" "$BODY" > "$EV/raw/x1_adjust.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/x1_adjust.json")"
BUD_V2=$(jq -r '.data.budget.id // empty' "$EV/raw/x1_adjust.json")
echo "new_version_id=$BUD_V2 $(jq -c '{budget_no:.data.budget.budget_no,version_no:.data.budget.version_no,change_type:.data.budget.change_type,status:.data.budget.status,total:.data.budget.total_amount,supersedes:.data.budget.supersedes_id}' "$EV/raw/x1_adjust.json")"
[ -n "$BUD_V2" ] || { echo "FATAL: 调整没建起来"; jq -c . "$EV/raw/x1_adjust.json"; exit 1; }
AINST=$(psqlat "select id from biz_flow_instance where biz_type = 'BUDGET_APPROVE' and biz_id = '$BUD_V2'::uuid order by created_at desc limit 1")
echo "adjust_instance=$AINST"
psqlq "select node_key, status from biz_flow_task where instance_id = '$AINST'::uuid order by created_at"
approve_instance "$AINST" "P0-06 预算追加同意"
sleep 2
psqlq "select budget_no, version_no, change_type, status, total_amount, effective_from, supersedes_id is not null as is_adjust from biz_budget where dept_id = '$DEPT'::uuid and account_id = '8c368d40-4c97-4f24-aa91-5562899014c3'::uuid and fiscal_year = 2026 order by version_no"

echo "===== ⑥-6 追加额度后重提被拦的 1000 单（EXP-20261107-000011，原 409）====="
api POST "/business/erp/expense/$R1000/submit" "$EMP" > "$EV/raw/x2_resubmit.json"
echo "submit http=$HTTP $(jq -c '{code,msg,status:.data.report.status,over_budget:.data.report.over_budget,over_amount:.data.report.over_amount,usages:(.data.budget_usages|length)}' "$EV/raw/x2_resubmit.json")"
psqlq "select report_no, status, over_budget, over_amount from biz_expense_report where id = '$R1000'::uuid"
psqlq "select source_no, period_code, amount, status, over_budget, budget_id from biz_budget_usage where source_id = '$R1000'::uuid"

echo "===== ⑥-7 重提后预算台账 / 执行报表（11000 / 已用 10500 / 剩余 500）====="
api GET "/business/erp/budget/$BUD_V2" "$ADMIN" > "$EV/raw/x3_detail_v2.json"
jq -c '{status:.data.budget.status,total:.data.budget.total_amount,year_used:.data.year_used,year_available:.data.year_available,p11:[.data.periods[]|select(.period_code=="2026-11")|{used,available}]}' "$EV/raw/x3_detail_v2.json"
api GET "/business/erp/budget/report?fiscal_year=2026&dept_id=$DEPT" "$ADMIN" > "$EV/raw/x4_report_after_adjust.json"
jq -c '.data[]|select(.account_code=="5602.02")|{total_amount,occupied,consumed,used,available,exec_ratio,over}' "$EV/raw/x4_report_after_adjust.json"
