#!/usr/bin/env bash
# ⑨ 金额三档（流程全配置化）再验两档：
#    < 1000  → 只走直属上级（1 个待办）
#    ≥ 10000 → 直属上级 + 财务经理/总经理会签（COUNTERSIGN，3 个待办）
#    12000 那单用 budget_controlled = 0 的费用类型（PENALTY / 5711 营业外支出），
#    以免与 ⑦ 的 8000 预算槽（BLOCK）混在一起 —— 本次只验审批路由，不验预算。
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

GM=$(login gm_drill "$DRILL_PWD")
echo "gm_drill token len=${#GM}"
echo "$GM" > "$(tokf gm)"
GMID=$(psqlat "select id from sys_user where username = 'gm_drill' and deleted_at is null")
PENALTY=$(psqlat "select id from biz_expense_type where type_code = 'PENALTY' and deleted_at is null")
echo "gm=$GMID penalty_type=$PENALTY"

approve_instance() { # $1=instance_id $2=comment
  local inst="$1" cmt="$2" i=0 line tid aid tok who
  while [ "$i" -lt 8 ]; do
    i=$((i + 1))
    line=$(psqlat "select t.id || '|' || t.assignee_id from biz_flow_task t where t.instance_id = '$inst'::uuid and t.status = 'PENDING' order by t.created_at limit 1")
    [ -n "$line" ] || { echo "  (无待办，收口)"; return 0; }
    tid="${line%%|*}"; aid="${line#*|}"
    case "$aid" in
      "$LEADID") tok="$LEAD"; who=lead_drill ;;
      "$FINID")  tok="$FIN";  who=fin_drill ;;
      "$GMID")   tok="$GM";   who=gm_drill ;;
      *)         tok="$ADMIN"; who=admin ;;
    esac
    api POST "/business/flow/task/$tid/approve" "$tok" "{\"comment\":\"$cmt（$who）\"}" > "/tmp/_p005_t_$i.json"
    echo "  approve task=$tid who=$who http=$HTTP $(jq -c '{code,msg,inst:.data.instance_status,node:.data.node_status,approver_count:.data.approver_count,approved_count:.data.approved_count}' "/tmp/_p005_t_$i.json")"
    sleep 1
  done
}

echo "===== ⑨-1 500 元单：只应生成 1 个待办（直属上级）====="
B1="{\"title\":\"P0-05 小额 500（< 1000 只走上级）\",\"expense_date\":\"2026-11-06\",\"dept_id\":\"$DEPT\",\"remark\":\"金额分档验证\",\"items\":[{\"expense_date\":\"2026-11-06\",\"expense_type_id\":\"$TYPE\",\"with_tax_amount\":\"500\",\"tax_rate\":\"0\",\"summary\":\"打车\",\"invoice_no\":\"INV-DRILL-0051\",\"invoice_date\":\"2026-11-06\",\"invoice_type\":\"VAT_NORMAL\"}]}"
api POST "/business/erp/expense" "$EMP" "$B1" > "$EV/raw/t1_create.json"
E1=$(jq -r '.data.report.id // empty' "$EV/raw/t1_create.json")
echo "create http=$HTTP report=$(jq -r '.data.report.report_no' "$EV/raw/t1_create.json") id=$E1"
api POST "/business/erp/expense/$E1/submit" "$EMP" > "$EV/raw/t1_submit.json"
echo "submit http=$HTTP $(jq -c '{code,msg,status:.data.report.status,tier:.data.tier_label,over_budget:.data.report.over_budget}' "$EV/raw/t1_submit.json")"
psqlq "select i.instance_no, i.status, (select count(*) from biz_flow_task t where t.instance_id = i.id) as tasks, (select string_agg(t.node_key, ',') from biz_flow_task t where t.instance_id = i.id) as nodes from biz_flow_instance i where i.biz_id = '$E1'::uuid"
I1=$(psqlat "select id from biz_flow_instance where biz_type = 'EXPENSE' and biz_id = '$E1'::uuid order by created_at desc limit 1")
approve_instance "$I1" "P0-05 小额同意"
sleep 2
psqlq "select report_no, status, over_budget, over_amount from biz_expense_report where id = '$E1'::uuid"

echo "===== ⑨-2 12000 元单：直属上级 + 会签（财务经理 + 总经理）====="
B2="{\"title\":\"P0-05 大额 12000（≥ 10000 会签）\",\"expense_date\":\"2026-11-06\",\"dept_id\":\"$DEPT\",\"remark\":\"会签档验证（类型 budget_controlled=0）\",\"items\":[{\"expense_date\":\"2026-11-06\",\"expense_type_id\":\"$PENALTY\",\"with_tax_amount\":\"12000\",\"tax_rate\":\"0\",\"summary\":\"合同违约金\",\"invoice_no\":\"INV-DRILL-0061\",\"invoice_date\":\"2026-11-06\",\"invoice_type\":\"VAT_NORMAL\"}]}"
api POST "/business/erp/expense" "$EMP" "$B2" > "$EV/raw/t2_create.json"
E2=$(jq -r '.data.report.id // empty' "$EV/raw/t2_create.json")
echo "create http=$HTTP report=$(jq -r '.data.report.report_no' "$EV/raw/t2_create.json") id=$E2"
api POST "/business/erp/expense/$E2/submit" "$EMP" > "$EV/raw/t2_submit.json"
echo "submit http=$HTTP $(jq -c '{code,msg,status:.data.report.status,tier:.data.tier_label,over_budget:.data.report.over_budget,budget_usages:(.data.budget_usages|length)}' "$EV/raw/t2_submit.json")"
psqlq "select i.instance_no, i.status, (select count(*) from biz_flow_task t where t.instance_id = i.id) as tasks, (select string_agg(t.node_key, ',') from biz_flow_task t where t.instance_id = i.id) as nodes from biz_flow_instance i where i.biz_id = '$E2'::uuid"
I2=$(psqlat "select id from biz_flow_instance where biz_type = 'EXPENSE' and biz_id = '$E2'::uuid order by created_at desc limit 1")
approve_instance "$I2" "P0-05 大额同意"
sleep 3
psqlq "select report_no, status, total_with_tax, over_budget from biz_expense_report where id = '$E2'::uuid"

echo "===== ⑨-3 两张单的凭证 ====="
psqlq "select v.voucher_no, v.source_type, v.source_no, v.total_debit, v.total_credit, (v.total_debit = v.total_credit) as balanced from biz_voucher v where v.source_no in (select report_no from biz_expense_report where id in ('$E1'::uuid, '$E2'::uuid)) order by v.voucher_no"
psqlq "select v.voucher_no, e.line_no, e.account_code, e.account_name, e.debit_amount, e.credit_amount from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id where v.source_no in (select report_no from biz_expense_report where id in ('$E1'::uuid, '$E2'::uuid)) order by v.voucher_no, e.line_no"
