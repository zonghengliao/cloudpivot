#!/usr/bin/env bash
# ⑦-2 续：预算审批走完（部门主管：demo_lead + lead_drill 都签；财务经理：fin_drill）→ ACTIVE
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh
DEMO=$(cat "$(tokf demo_lead)")
LEAD=$(cat "$(tokf lead_drill)")
FIN=$(cat "$(tokf fin_drill)")
BUD=$(cat "$EV/raw/budget.id")
BINST=$(psqlat "select id from biz_flow_instance where biz_type = 'BUDGET_APPROVE' and biz_id = '$BUD'::uuid order by created_at desc limit 1")
echo "budget_instance=$BINST"

pending() { psqlat "select t.id || '|' || t.node_key || '|' || coalesce(t.assignee_name,'') from biz_flow_task t where t.instance_id = '$BINST'::uuid and t.status = 'PENDING' order by t.created_at limit 1"; }

N=0
while [ "$N" -lt 6 ]; do
  N=$((N + 1))
  LINE=$(pending)
  [ -n "$LINE" ] || { echo "  (无待办，收口)"; break; }
  TID="${LINE%%|*}"; REST="${LINE#*|}"; NODE="${REST%%|*}"; WHO="${REST#*|}"
  case "$WHO" in
    "演练审批人")   TOK="$DEMO" ;;   # demo_lead（DEPT_LEADER 的另一个成员）
    "演练直属上级") TOK="$LEAD" ;;
    "演练财务经理") TOK="$FIN" ;;
    *)              TOK="$FIN" ;;
  esac
  api POST "/business/flow/task/$TID/approve" "$TOK" "{\"comment\":\"P0-05 预算演练同意（$WHO）\"}" > "$EV/raw/b3_ap_$N.json"
  echo "  approve node=$NODE who=$WHO http=$HTTP $(jq -c '{code,msg,inst:.data.instance_status,node:.data.node_status,done:.data.done,approver_count:.data.approver_count,approved_count:.data.approved_count}' "$EV/raw/b3_ap_$N.json")"
  sleep 1
done
sleep 2

echo "===== 预算最终状态 ====="
cat > /tmp/p005_budget_active.sql <<SQL
\pset pager off
select budget_no, status, control_mode, total_amount, effective_from, approved_at, approved_by, approval_instance_id
  from biz_budget where id = '$BUD'::uuid;
select instance_no, biz_type, status, ended_at from biz_flow_instance where id = '$BINST'::uuid;
select node_key, assignee_name, status, comment from biz_flow_task where instance_id = '$BINST'::uuid order by created_at;
SQL
psqlf /tmp/p005_budget_active.sql
