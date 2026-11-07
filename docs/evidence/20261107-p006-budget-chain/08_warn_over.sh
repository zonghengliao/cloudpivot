#!/usr/bin/env bash
# ⑥ 超预算「放行」路径实测（WARN 策略）：1000 的预算报 3500 → 标记 over_budget 并走同一条审批链
#    同时验证：审批流里没有「超预算」独立节点（节点表已证：expense 流 7 节点无 over_budget 分支）
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh

ADMIN=$(cat "$(tokf admin)")
EMP=$(cat "$(tokf emp_drill)")
DEPT=$(cat "$EV/raw/dept.id")
ACC_OFFICE='60698750-124d-489f-9f39-4a355b56e7e2'   # 5602.03 管理费用-办公费（末级）
TYPE_OFFICE=$(psqlat "select id from biz_expense_type where type_code = 'OFFICE' and deleted_at is null")
echo "dept=$DEPT type_office=$TYPE_OFFICE acc_office=$ACC_OFFICE"

echo "===== ⑥-1 编一张 WARN 策略的预算：2026 / DRILL_BDG / 5602.03 / 1000 ====="
PERIODS='[{"month":1,"amount":"0"},{"month":2,"amount":"0"},{"month":3,"amount":"0"},{"month":4,"amount":"0"},{"month":5,"amount":"0"},{"month":6,"amount":"0"},{"month":7,"amount":"0"},{"month":8,"amount":"0"},{"month":9,"amount":"0"},{"month":10,"amount":"0"},{"month":11,"amount":"1000"},{"month":12,"amount":"0"}]'
BODY="{\"fiscal_year\":2026,\"dept_id\":\"$DEPT\",\"account_id\":\"$ACC_OFFICE\",\"total_amount\":\"1000\",\"control_mode\":\"WARN\",\"periods\":$PERIODS,\"remark\":\"P0-06 超预算放行演练（WARN）\"}"
api POST "/business/erp/budget" "$ADMIN" "$BODY" > "$EV/raw/w1_create.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/w1_create.json")"
BUD2=$(jq -r '.data.budget.id // empty' "$EV/raw/w1_create.json")
echo "$BUD2" > "$EV/raw/budget_warn.id"
[ -n "$BUD2" ] || { echo "FATAL: WARN 预算没建起来"; jq -c . "$EV/raw/w1_create.json"; exit 1; }
api POST "/business/erp/budget/$BUD2/submit" "$ADMIN" > "$EV/raw/w2_submit.json"
echo "submit http=$HTTP $(jq -c '{code,msg,status:.data.budget.status}' "$EV/raw/w2_submit.json")"
BINST2=$(psqlat "select id from biz_flow_instance where biz_type = 'BUDGET_APPROVE' and biz_id = '$BUD2'::uuid order by created_at desc limit 1")
approve_instance "$BINST2" "P0-06 WARN 预算同意"
sleep 2
psqlq "select budget_no, status, control_mode, total_amount from biz_budget where id = '$BUD2'::uuid"

echo "===== ⑥-2 超预算报销 3500（预算 1000）→ WARN 下应放行并打标 ====="
EBODY="{\"title\":\"P0-06 超预算放行 3500（办公费）\",\"expense_date\":\"2026-11-10\",\"dept_id\":\"$DEPT\",\"remark\":\"WARN 策略下应放行并标记\",\"items\":[{\"expense_date\":\"2026-11-10\",\"expense_type_id\":\"$TYPE_OFFICE\",\"with_tax_amount\":\"3500\",\"tax_rate\":\"0\",\"summary\":\"办公耗材\",\"invoice_no\":\"INV-P006-OFFICE\",\"invoice_date\":\"2026-11-10\",\"invoice_type\":\"VAT_NORMAL\"}]}"
api POST "/business/erp/expense" "$EMP" "$EBODY" > "$EV/raw/w3_create.json"
EID=$(jq -r '.data.report.id // empty' "$EV/raw/w3_create.json")
echo "create http=$HTTP $(jq -c '{code,msg,report_no:.data.report.report_no,status:.data.report.status,total:.data.report.total_with_tax}' "$EV/raw/w3_create.json")"
echo "$EID" > "$EV/raw/report_warn.id"
api POST "/business/erp/expense/$EID/submit" "$EMP" > "$EV/raw/w4_submit.json"
echo "submit http=$HTTP $(jq -c '{code,msg,status:.data.report.status,over_budget:.data.report.over_budget,over_amount:.data.report.over_amount,mode:.data.report.budget_mode,usages:(.data.budget_usages|length)}' "$EV/raw/w4_submit.json")"
psqlq "select report_no, status, over_budget, over_amount, budget_mode from biz_expense_report where id = '$EID'::uuid"
psqlq "select source_no, period_code, amount, status, over_budget, over_amount, budget_id from biz_budget_usage where source_id = '$EID'::uuid"

echo "===== ⑥-3 超预算单的审批链 vs 普通单的审批链（是否多一道「超预算」审批）====="
WINST=$(psqlat "select id from biz_flow_instance where biz_type = 'EXPENSE' and biz_id = '$EID'::uuid order by created_at desc limit 1")
echo "warn_instance=$WINST"
psqlq "select 'warn单' as which, node_key, status from biz_flow_task where instance_id = '$WINST'::uuid order by created_at"
NINST=$(psqlat "select id from biz_flow_instance where biz_type = 'EXPENSE' and biz_id = '$(cat "$EV/raw/r3500.id")'::uuid order by created_at desc limit 1")
psqlq "select '普通单' as which, node_key, status from biz_flow_task where instance_id = '$NINST'::uuid order by created_at"
echo "--- 流程定义节点（expense 流全量，7 节点，无条件分支引用 over_budget）---"
psqlat "select string_agg(node_key || ':' || node_type, ' | ' order by node_key) from biz_flow_node where version_id = (select v.id from biz_flow_def d join biz_flow_def_version v on v.def_id=d.id where d.flow_code='expense' and v.is_current)"

echo "===== ⑥-4 走完审批 → 放行落账（超预算单也要能批完，才会暴露给财务）====="
approve_instance "$WINST" "P0-06 超预算同意（WARN 放行）"
sleep 2
psqlq "select report_no, status, over_budget, over_amount from biz_expense_report where id = '$EID'::uuid"
psqlq "select source_no, amount, status, over_budget, over_amount from biz_budget_usage where source_id = '$EID'::uuid"
psqlq "select v.voucher_no, v.source_type, v.total_debit, v.total_credit, v.status from biz_voucher v where v.source_id = '$EID'::uuid"
echo "--- 该槽预算执行（1000 额度 / 3500 已用 → 应 available = -2500、over = true）---"
api GET "/business/erp/budget/report?fiscal_year=2026&dept_id=$DEPT" "$ADMIN" > "$EV/raw/w5_report.json"
jq -c '.data[]|select(.account_code=="5602.03")|{account_code,total_amount,occupied,consumed,used,available,exec_ratio,over}' "$EV/raw/w5_report.json"
