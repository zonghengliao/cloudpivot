#!/usr/bin/env bash
# ③④ 补完：把 3500 / 6000 两张报销单走完审批 → 占用转消耗（used 口径不变），并核对凭证与单据状态
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh

ADMIN=$(cat "$(tokf admin)")
DEPT=$(cat "$EV/raw/dept.id")
BUD=$(cat "$EV/raw/budget.id")
ACC_TRAVEL='8c368d40-4c97-4f24-aa91-5562899014c3'

approve_report() { # $1=tag
  local tag="$1" id inst
  id=$(cat "$EV/raw/${tag}.id")
  inst=$(psqlat "select id from biz_flow_instance where biz_type = 'EXPENSE' and biz_id = '$id'::uuid order by created_at desc limit 1")
  echo "===== $tag 审批：instance=$inst ====="
  [ -n "$inst" ] || { echo "  FATAL 无流程实例"; return 1; }
  psqlq "select node_key, status, assignee_id from biz_flow_task where instance_id = '$inst'::uuid order by created_at"
  approve_instance "$inst" "P0-06 $tag 同意"
  sleep 2
  echo "--- $tag 审批后：单据 / 占用 / 凭证 ---"
  psqlq "select report_no, status, over_budget, over_amount, approval_instance_id is not null as has_inst from biz_expense_report where id = '$id'::uuid"
  psqlq "select source_no, period_code, amount, status, over_budget from biz_budget_usage where source_id = '$id'::uuid"
  psqlq "select v.voucher_no, v.source_type, v.total_debit, v.total_credit, v.balanced from biz_voucher v where v.source_type = 'EXPENSE_REPORT' and v.source_id = '$id'::uuid"
}

approve_report r3500
approve_report r6000

echo "===== 审批完成后预算台账复核（used 应仍 = 9500：OCCUPIED → CONSUMED 口径不变）====="
api GET "/business/erp/budget/$BUD" "$ADMIN" > "$EV/raw/b4_detail_after_approve.json"
jq -c '{status:.data.budget.status,year_used:.data.year_used,year_available:.data.year_available,usages:[.data.usages[]|{source_no,amount,status}]}' "$EV/raw/b4_detail_after_approve.json"
psqlq "select status, count(*), coalesce(sum(amount),0) as amount from biz_budget_usage where dept_id = '$DEPT'::uuid and account_id = '$ACC_TRAVEL'::uuid and fiscal_year = 2026 and deleted_at is null group by status order by status"
