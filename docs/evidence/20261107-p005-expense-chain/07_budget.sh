#!/usr/bin/env bash
# ⑦ 预算控制：编 8000 预算（BLOCK）→ 审批到 ACTIVE → 预算内单占用预算 → 超预算单被拦截
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
ADMIN=$(cat "$(tokf admin)")
EMP=$(cat "$(tokf emp_drill)")
LEAD=$(cat "$(tokf lead_drill)")
FIN=$(cat "$(tokf fin_drill)")
DEPT=$(cat "$EV/raw/dept.id")
TYPE=$(cat "$EV/raw/type.id")
LEADID=$(cat "$EV/raw/lead_drill.id")
FINID=$(cat "$EV/raw/fin_drill.id")

# 按实例的待办把审批逐节点走完（审批人用库里 task.assignee_id 对应的令牌）
approve_instance() { # $1=instance_id $2=comment
  local inst="$1" cmt="$2" i=0 tid aid tok
  while [ "$i" -lt 6 ]; do
    i=$((i + 1))
    tid=$(psqlat "select t.id || '|' || t.assignee_id from biz_flow_task t where t.instance_id = '$inst'::uuid and t.status = 'PENDING' order by t.created_at limit 1")
    [ -n "$tid" ] || { echo "  (实例 $inst 已无待办)"; return 0; }
    aid="${tid#*|}"
    tid="${tid%%|*}"
    case "$aid" in
      "$LEADID") tok="$LEAD"; who=lead_drill ;;
      "$FINID")  tok="$FIN";  who=fin_drill ;;
      *)         tok="$ADMIN"; who=admin ;;
    esac
    api POST "/business/flow/task/$tid/approve" "$tok" "{\"comment\":\"$cmt（$who）\"}" > "/tmp/_p005_ap_$i.json"
    echo "  approve task=$tid who=$who http=$HTTP $(jq -c '{code,msg}' "/tmp/_p005_ap_$i.json")"
    sleep 1
  done
}

echo "===== ⑦-1 编制预算 8000（部门 DRILL_EXP / 科目 5602.02 / 槽级策略 BLOCK / 2026-11 月 8000）====="
PERIODS='[{"month":1,"amount":"0"},{"month":2,"amount":"0"},{"month":3,"amount":"0"},{"month":4,"amount":"0"},{"month":5,"amount":"0"},{"month":6,"amount":"0"},{"month":7,"amount":"0"},{"month":8,"amount":"0"},{"month":9,"amount":"0"},{"month":10,"amount":"0"},{"month":11,"amount":"8000"},{"month":12,"amount":"0"}]'
BODY="{\"fiscal_year\":2026,\"dept_id\":\"$DEPT\",\"account_id\":\"$ACC_TRAVEL\",\"total_amount\":\"8000\",\"control_mode\":\"BLOCK\",\"periods\":$PERIODS,\"remark\":\"P0-05 预算控制演练\"}"
api POST "/business/erp/budget" "$ADMIN" "$BODY" > "$EV/raw/b1_create.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/b1_create.json")"
BUD=$(jq -r '.data.id // empty' "$EV/raw/b1_create.json")
if [ -z "$BUD" ]; then
  api GET "/business/erp/budget/page?page=1&page_size=50" "$ADMIN" > "$EV/raw/b1_list.json"
  BUD=$(jq -r '[.data.list[]?|select(.remark=="P0-05 预算控制演练")][0].id // empty' "$EV/raw/b1_list.json")
fi
echo "budget_id=$BUD"
echo "$BUD" > "$EV/raw/budget.id"
[ -n "$BUD" ] || { echo "FATAL: 预算没建起来"; exit 1; }
jq -c '{budget_no:.data.budget_no,status:.data.status,total_amount:.data.total_amount,control_mode:.data.control_mode,dept_name:.data.dept_name,account_name:.data.account_name}' "$EV/raw/b1_create.json"

echo "===== ⑦-2 预算提交 + 走审批（部门主管 → 财务经理）====="
api POST "/business/erp/budget/$BUD/submit" "$ADMIN" > "$EV/raw/b2_submit.json"
echo "submit http=$HTTP $(jq -c '{code,msg,status:.data.status}' "$EV/raw/b2_submit.json")"
BINST=$(psqlat "select id from biz_flow_instance where biz_type = 'BUDGET_APPROVE' and biz_id = '$BUD'::uuid order by created_at desc limit 1")
echo "budget_instance=$BINST"
[ -n "$BINST" ] || { echo "FATAL: 预算审批实例没起来"; exit 1; }
approve_instance "$BINST" "P0-05 预算演练同意"
sleep 2

echo "===== ⑦-3 预算内报销 2000：提交 → 占用预算 → 审批 → 消耗 ====="
BODY2="{\"title\":\"P0-05 预算内报销 2000\",\"expense_date\":\"2026-11-02\",\"dept_id\":\"$DEPT\",\"remark\":\"预算内\",\"items\":[{\"expense_date\":\"2026-11-02\",\"expense_type_id\":\"$TYPE\",\"with_tax_amount\":\"2000\",\"tax_rate\":\"0\",\"summary\":\"高铁票\",\"invoice_no\":\"INV-DRILL-0011\",\"invoice_date\":\"2026-11-02\",\"invoice_type\":\"VAT_NORMAL\"}]}"
api POST "/business/erp/expense" "$EMP" "$BODY2" > "$EV/raw/e1_create.json"
echo "create http=$HTTP $(jq -c '{code,msg,report_no:.data.report.report_no,status:.data.report.status}' "$EV/raw/e1_create.json")"
E1=$(jq -r '.data.report.id // empty' "$EV/raw/e1_create.json")
echo "$E1" > "$EV/raw/report_in.id"

api POST "/business/erp/expense/$E1/submit" "$EMP" > "$EV/raw/e2_submit.json"
echo "submit http=$HTTP $(jq -c '{code,msg,status:.data.report.status,over_budget:.data.report.over_budget,over_amount:.data.report.over_amount,budget_usages:(.data.budget_usages|length)}' "$EV/raw/e2_submit.json")"
psqlq "select u.source_no, u.amount, u.status, u.over_budget, u.budget_id from biz_budget_usage u where u.source_id = '$E1'::uuid"

E1INST=$(psqlat "select id from biz_flow_instance where biz_type = 'EXPENSE' and biz_id = '$E1'::uuid order by created_at desc limit 1")
approve_instance "$E1INST" "P0-05 预算内报销同意"
sleep 2
psqlq "select report_no, status, over_budget, over_amount, budget_mode from biz_expense_report where id = '$E1'::uuid"
psqlq "select u.source_no, u.amount, u.status, u.over_budget, u.budget_id from biz_budget_usage u where u.source_id = '$E1'::uuid"

echo "===== ⑦-4 超预算报销 3000（已用 5500 + 3000 > 8000）：应被拦截 ====="
BODY3="{\"title\":\"P0-05 超预算报销 3000\",\"expense_date\":\"2026-11-03\",\"dept_id\":\"$DEPT\",\"remark\":\"应被预算拦截\",\"items\":[{\"expense_date\":\"2026-11-03\",\"expense_type_id\":\"$TYPE\",\"with_tax_amount\":\"3000\",\"tax_rate\":\"0\",\"summary\":\"往返机票\",\"invoice_no\":\"INV-DRILL-0021\",\"invoice_date\":\"2026-11-03\",\"invoice_type\":\"VAT_NORMAL\"}]}"
api POST "/business/erp/expense" "$EMP" "$BODY3" > "$EV/raw/e3_create.json"
echo "create http=$HTTP $(jq -c '{code,msg,report_no:.data.report.report_no}' "$EV/raw/e3_create.json")"
E3=$(jq -r '.data.report.id // empty' "$EV/raw/e3_create.json")
echo "$E3" > "$EV/raw/report_over.id"

api POST "/business/erp/expense/$E3/submit" "$EMP" > "$EV/raw/e4_submit_blocked.json"
echo "submit http=$HTTP"
jq -c '{code,msg}' "$EV/raw/e4_submit_blocked.json"
echo "--- 被拦截后：单据仍是草稿、没有占用行、没有流程实例 ---"
psqlq "select report_no, status, over_budget, over_amount, budget_mode, approval_instance_id from biz_expense_report where id = '$E3'::uuid"
psqlq "select count(*) as usage_rows from biz_budget_usage where source_id = '$E3'::uuid"
psqlq "select count(*) as instances from biz_flow_instance where biz_id = '$E3'::uuid"

echo "===== ⑦-5 预算台账（额度 / 已用 / 占用 / 消耗）====="
cat > /tmp/p005_budget_verify.sql <<SQL
\pset pager off
select b.budget_no, b.status, b.fiscal_year, b.dept_name, b.account_code, b.control_mode, b.total_amount,
       b.effective_from
  from biz_budget b where b.id = '$BUD'::uuid;
\echo --- 月度分解（只看 2026-11）---
select period_code, period_month, amount from biz_budget_period where budget_id = '$BUD'::uuid and period_code = '2026-11';
\echo --- 占用台账（该槽全部行）---
select u.source_no, u.period_code, u.amount, u.status, u.over_budget, u.budget_id
  from biz_budget_usage u
 where u.dept_id = '$DEPT'::uuid and u.account_id = '$ACC_TRAVEL'::uuid and u.period_code = '2026-11'
 order by u.created_at;
\echo --- 槽汇总：已用（OCCUPIED + CONSUMED）---
select coalesce(sum(case when status in ('OCCUPIED','CONSUMED') then amount end), 0) as used,
       coalesce(sum(case when status = 'CONSUMED' then amount end), 0) as consumed,
       coalesce(sum(case when status = 'OCCUPIED' then amount end), 0) as occupied
  from biz_budget_usage
 where dept_id = '$DEPT'::uuid and account_id = '$ACC_TRAVEL'::uuid and period_code = '2026-11';
\echo --- 预算执行报表（接口同源）---
select b.budget_no, b.total_amount, r.*
  from biz_budget b left join lateral (select 1) r on true where b.id = '$BUD'::uuid;
SQL
psqlf /tmp/p005_budget_verify.sql
