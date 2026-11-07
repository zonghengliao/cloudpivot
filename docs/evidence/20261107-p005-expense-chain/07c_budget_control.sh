#!/usr/bin/env bash
# ⑦-3 预算转为 ACTIVE 之后的控制验证：
#   a) 作废上一轮「未被拦住」的 3000 单（释放占用）
#   b) 预算内 1000 单：提交 → OCCUPIED；撤回 → RELEASED
#   c) 超预算 3000 单：提交被 409 拦截（ERP_BUDGET_EXCEEDED）
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
ADMIN=$(cat "$(tokf admin)")
EMP=$(cat "$(tokf emp_drill)")
DEPT=$(cat "$EV/raw/dept.id")
TYPE=$(cat "$EV/raw/type.id")
OLD=$(cat "$EV/raw/report_over.id")

echo "===== ⑦-3a 作废上一轮的 3000 草稿单（EXP-20261107-000004，释放 3000 占用）====="
api POST "/business/erp/expense/$OLD/cancel" "$ADMIN" '{"reason":"P0-05 预算未 ACTIVE 时未被拦截的单据，作废重做"}' > "$EV/raw/c1_cancel.json"
echo "http=$HTTP $(jq -c '{code,msg,status:.data.report.status}' "$EV/raw/c1_cancel.json")"
psqlq "select source_no, amount, status, released_at, release_reason from biz_budget_usage where source_id = '$OLD'::uuid"

echo "===== ⑦-3b 预算内 1000 单：提交 → 占用 → 撤回 → 释放 ====="
BODY="{\"title\":\"P0-05 预算内报销 1000\",\"expense_date\":\"2026-11-04\",\"dept_id\":\"$DEPT\",\"remark\":\"预算内 1000\",\"items\":[{\"expense_date\":\"2026-11-04\",\"expense_type_id\":\"$TYPE\",\"with_tax_amount\":\"1000\",\"tax_rate\":\"0\",\"summary\":\"市内交通\",\"invoice_no\":\"INV-DRILL-0031\",\"invoice_date\":\"2026-11-04\",\"invoice_type\":\"VAT_NORMAL\"}]}"
api POST "/business/erp/expense" "$EMP" "$BODY" > "$EV/raw/c2_create.json"
R1000=$(jq -r '.data.report.id // empty' "$EV/raw/c2_create.json")
echo "create http=$HTTP report_no=$(jq -r '.data.report.report_no' "$EV/raw/c2_create.json") id=$R1000"
echo "$R1000" > "$EV/raw/report_ok.id"

api POST "/business/erp/expense/$R1000/submit" "$EMP" > "$EV/raw/c3_submit.json"
echo "submit http=$HTTP $(jq -c '{code,msg,status:.data.report.status,over_budget:.data.report.over_budget,over_amount:.data.report.over_amount,budget_mode:.data.report.budget_mode}' "$EV/raw/c3_submit.json")"
psqlq "select source_no, amount, status, over_budget, budget_id is not null as has_budget_id from biz_budget_usage where source_id = '$R1000'::uuid"

echo "--- 撤回（释放占用）---"
api POST "/business/erp/expense/$R1000/withdraw" "$EMP" > "$EV/raw/c4_withdraw.json"
echo "withdraw http=$HTTP $(jq -c '{code,msg,status:.data.report.status}' "$EV/raw/c4_withdraw.json")"
psqlq "select source_no, amount, status, released_at, release_reason from biz_budget_usage where source_id = '$R1000'::uuid"

echo "===== ⑦-3c 超预算 3000 单：提交应被拦截 ====="
BODY2="{\"title\":\"P0-05 超预算报销 3000\",\"expense_date\":\"2026-11-05\",\"dept_id\":\"$DEPT\",\"remark\":\"超预算应被拦截\",\"items\":[{\"expense_date\":\"2026-11-05\",\"expense_type_id\":\"$TYPE\",\"with_tax_amount\":\"3000\",\"tax_rate\":\"0\",\"summary\":\"往返机票\",\"invoice_no\":\"INV-DRILL-0041\",\"invoice_date\":\"2026-11-05\",\"invoice_type\":\"VAT_NORMAL\"}]}"
api POST "/business/erp/expense" "$EMP" "$BODY2" > "$EV/raw/c5_create.json"
ROVER=$(jq -r '.data.report.id // empty' "$EV/raw/c5_create.json")
echo "create http=$HTTP report_no=$(jq -r '.data.report.report_no' "$EV/raw/c5_create.json") id=$ROVER"
echo "$ROVER" > "$EV/raw/report_over2.id"

api POST "/business/erp/expense/$ROVER/submit" "$EMP" > "$EV/raw/c6_submit_blocked.json"
echo "submit http=$HTTP"
jq -c '{code,msg}' "$EV/raw/c6_submit_blocked.json"
echo "--- 被拦截后：单据仍草稿 / 无占用行 / 无审批实例 ---"
psqlq "select report_no, status, over_budget, over_amount, budget_mode, approval_instance_id from biz_expense_report where id = '$ROVER'::uuid"
psqlq "select count(*) as usage_rows from biz_budget_usage where source_id = '$ROVER'::uuid"
psqlq "select count(*) as instances from biz_flow_instance where biz_id = '$ROVER'::uuid"

echo "===== ⑦-4 预算槽台账（重算已用）====="
cat > /tmp/p005_budget_slot.sql <<SQL
\pset pager off
select b.budget_no, b.status, b.control_mode, b.total_amount, b.effective_from
  from biz_budget b where b.dept_id = '$DEPT'::uuid and b.account_id = '$ACC_TRAVEL'::uuid and b.fiscal_year = 2026;
\echo --- 占用台账（该槽 2026-11 全部行）---
select u.source_no, u.amount, u.status, u.over_budget, u.released_at is not null as released
  from biz_budget_usage u
 where u.dept_id = '$DEPT'::uuid and u.account_id = '$ACC_TRAVEL'::uuid and u.period_code = '2026-11'
 order by u.created_at;
\echo --- 汇总：额度 vs 已用 vs 可用 ---
select b.total_amount as year_limit,
       coalesce(u.used, 0) as year_used,
       b.total_amount - coalesce(u.used, 0) as year_available
  from biz_budget b
  left join (select sum(amount) as used from biz_budget_usage
              where dept_id = '$DEPT'::uuid and account_id = '$ACC_TRAVEL'::uuid
                and fiscal_year = 2026 and status in ('OCCUPIED','CONSUMED') and deleted_at is null) u on true
 where b.dept_id = '$DEPT'::uuid and b.account_id = '$ACC_TRAVEL'::uuid and b.status = 'ACTIVE';
SQL
psqlf /tmp/p005_budget_slot.sql
