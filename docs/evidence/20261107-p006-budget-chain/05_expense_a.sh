#!/usr/bin/env bash
# ③ 预算内报销 3500 ④ 临界报销 6000（累计 9500）⑤ 超支报销 1000（累计 10500 > 10000，应被拦截）
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh

ADMIN=$(cat "$(tokf admin)")
EMP=$(cat "$(tokf emp_drill)")
DEPT=$(cat "$EV/raw/dept.id")
BUD=$(cat "$EV/raw/budget.id")
TYPE=$(psqlat "select id from biz_expense_type where type_code = 'DRILL_TRAVEL' and deleted_at is null")
ACC_TRAVEL='8c368d40-4c97-4f24-aa91-5562899014c3'
echo "dept=$DEPT budget=$BUD type=$TYPE"

slot_used() {
  psqlat "select coalesce(sum(amount) filter (where status in ('OCCUPIED','CONSUMED')),0) from biz_budget_usage where dept_id = '$DEPT'::uuid and account_id = '$ACC_TRAVEL'::uuid and fiscal_year = 2026 and deleted_at is null"
}

# mkexpense <tag> <amount> <date> <title> <summary> <invoice_no>
mkexpense() {
  local tag="$1" amt="$2" day="$3" title="$4" summary="$5" inv="$6"
  local body="{\"title\":\"$title\",\"expense_date\":\"$day\",\"dept_id\":\"$DEPT\",\"remark\":\"P0-06 预算演练\",\"items\":[{\"expense_date\":\"$day\",\"expense_type_id\":\"$TYPE\",\"with_tax_amount\":\"$amt\",\"tax_rate\":\"0\",\"summary\":\"$summary\",\"invoice_no\":\"$inv\",\"invoice_date\":\"$day\",\"invoice_type\":\"VAT_NORMAL\"}]}"
  api POST "/business/erp/expense" "$EMP" "$body" > "$EV/raw/${tag}_create.json"
  local id; id=$(jq -r '.data.report.id // empty' "$EV/raw/${tag}_create.json")
  echo "  [$tag] create http=$HTTP $(jq -c '{code,msg,report_no:.data.report.report_no,status:.data.report.status,total:.data.report.total_with_tax}' "$EV/raw/${tag}_create.json")"
  [ -n "$id" ] || { echo "  [$tag] FATAL 建单失败"; return 1; }
  printf '%s' "$id" > "$EV/raw/${tag}.id"
  api POST "/business/erp/expense/$id/submit" "$EMP" > "$EV/raw/${tag}_submit.json"
  local sh="$HTTP"
  echo "  [$tag] submit http=$sh $(jq -c '{code,msg,status:.data.report.status,over_budget:.data.report.over_budget,over_amount:.data.report.over_amount,mode:.data.report.budget_mode,usages:(.data.budget_usages|length)}' "$EV/raw/${tag}_submit.json")"
  echo "  [$tag] DB: $(psqlat "select status || ' over=' || over_budget from biz_expense_report where id = '$id'::uuid") | usage=$(psqlat "select coalesce(string_agg(status || ':' || amount, ','),'-') from biz_budget_usage where source_id = '$id'::uuid") | inst=$(psqlat "select count(*) from biz_flow_instance where biz_id = '$id'::uuid") | slot_used=$(slot_used)"
  return 0
}

budget_view() { # 预算详情：已用 / 剩余 / 11 月
  api GET "/business/erp/budget/$BUD" "$ADMIN" > /tmp/_p006_detail.json
  echo "  budget http=$HTTP $(jq -c '{status:.data.budget.status,year_used:.data.year_used,year_available:.data.year_available,p11:[.data.periods[]|select(.period_code=="2026-11")|{used,available}]}' /tmp/_p006_detail.json)"
}

echo "===== ② 起点：预算 10000，已用 $(slot_used) ====="
budget_view

echo "===== ③ 预算内报销 3500（差旅费 2026-11-01）====="
mkexpense r3500 3500 2026-11-01 "P0-06 预算内报销 3500" "高铁票" "INV-P006-3500"
budget_view

echo "===== ④ 临界报销 6000（差旅费 2026-11-05，累计 9500）====="
mkexpense r6000 6000 2026-11-05 "P0-06 临界报销 6000" "机票" "INV-P006-6000"
budget_view

echo "===== ⑤ 超支报销 1000（差旅费 2026-11-08，累计 10500 > 10000）====="
mkexpense r1000 1000 2026-11-08 "P0-06 超支报销 1000" "出租车" "INV-P006-1000" || true
echo "--- ⑤ 报文全文 ---"; jq -c . "$EV/raw/r1000_submit.json"
echo "--- ⑤ DB 复核：单据仍是草稿？0 占用行？0 实例？预算已用未变？ ---"
R1000=$(cat "$EV/raw/r1000.id")
psqlq "select report_no, status, over_budget, over_amount, approval_instance_id from biz_expense_report where id = '$R1000'::uuid"
psqlat "select 'usage_rows=' || count(*) from biz_budget_usage where source_id = '$R1000'::uuid"
psqlat "select 'instance_rows=' || count(*) from biz_flow_instance where biz_id = '$R1000'::uuid"
psqlat "select 'slot_used=' || coalesce(sum(amount) filter (where status in ('OCCUPIED','CONSUMED')),0) from biz_budget_usage where dept_id = '$DEPT'::uuid and account_id = '$ACC_TRAVEL'::uuid and fiscal_year = 2026 and deleted_at is null"
budget_view
