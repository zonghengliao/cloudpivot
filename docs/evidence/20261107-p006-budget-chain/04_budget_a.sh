#!/usr/bin/env bash
# ① 编制预算 10000（部门 DRILL_BDG / 科目 5602.02 差旅费 / 2026 / 槽级策略 BLOCK / 11 月 10000）
# ② 查预算余额：已用 0 / 剩余 10000
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh

ADMIN=$(cat "$(tokf admin)")
DEPT=$(cat "$EV/raw/dept.id")
ACC_TRAVEL='8c368d40-4c97-4f24-aa91-5562899014c3'   # 5602.02 管理费用-差旅费（末级）

# ---- 按待办逐节点审批：assignee_id → 账号 → 令牌 ----
tok_by_user() {
  case "$1" in
    lead_drill|fin_drill|emp_drill|demo_lead|gm_drill|admin) cat "$(tokf "$1")" ;;
    *) printf '' ;;
  esac
}
approve_instance() { # $1=instance_id $2=comment
  local inst="$1" cmt="$2" i=0 tid aid uname tok
  while [ "$i" -lt 8 ]; do
    i=$((i + 1))
    tid=$(psqlat "select t.id || '|' || t.assignee_id from biz_flow_task t where t.instance_id = '$inst'::uuid and t.status = 'PENDING' order by t.created_at limit 1")
    [ -n "$tid" ] || { echo "  (实例 $inst 已无待办)"; return 0; }
    aid="${tid#*|}"; tid="${tid%%|*}"
    uname=$(psqlat "select username from sys_user where id = '$aid'::uuid")
    tok=$(tok_by_user "$uname")
    if [ -z "$tok" ]; then echo "  !! 待办 $tid 的审批人 $aid ($uname) 没有可用令牌"; return 1; fi
    api POST "/business/flow/task/$tid/approve" "$tok" "{\"comment\":\"$cmt（$uname）\"}" > "/tmp/_p006_ap_$i.json"
    echo "  approve task=$tid who=$uname http=$HTTP $(jq -c '{code,msg}' "/tmp/_p006_ap_$i.json")"
    sleep 1
  done
}

echo "===== ①-1 编制预算：2026 / DRILL_BDG / 5602.02 / 10000 / BLOCK / 11 月 10000 ====="
PERIODS='[{"month":1,"amount":"0"},{"month":2,"amount":"0"},{"month":3,"amount":"0"},{"month":4,"amount":"0"},{"month":5,"amount":"0"},{"month":6,"amount":"0"},{"month":7,"amount":"0"},{"month":8,"amount":"0"},{"month":9,"amount":"0"},{"month":10,"amount":"0"},{"month":11,"amount":"10000"},{"month":12,"amount":"0"}]'
BODY="{\"fiscal_year\":2026,\"dept_id\":\"$DEPT\",\"account_id\":\"$ACC_TRAVEL\",\"total_amount\":\"10000\",\"control_mode\":\"BLOCK\",\"periods\":$PERIODS,\"remark\":\"P0-06 预算控制链演练：差旅费年度 10000\"}"
api POST "/business/erp/budget" "$ADMIN" "$BODY" > "$EV/raw/b1_create.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/b1_create.json")"
BUD=$(jq -r '.data.budget.id // empty' "$EV/raw/b1_create.json")
echo "$BUD" > "$EV/raw/budget.id"
[ -n "$BUD" ] || { echo "FATAL: 预算没建起来"; jq -c . "$EV/raw/b1_create.json"; exit 1; }
jq -c '{budget_no:.data.budget.budget_no,status:.data.budget.status,total_amount:.data.budget.total_amount,control_mode:.data.budget.control_mode,version_no:.data.budget.version_no,dept:.data.budget.dept_name,account:.data.budget.account_code,year_used:.data.year_used,year_available:.data.year_available,periods:(.data.periods|length)}' "$EV/raw/b1_create.json"
echo "--- DB 落库（biz_budget + 12 行月度分解）---"
psqlq "select budget_no, fiscal_year, dept_name, account_code, control_mode, total_amount, status, version_no, effective_from from biz_budget where id = '$BUD'::uuid"
psqlat "select count(*) from biz_budget_period where budget_id = '$BUD'::uuid"

echo "===== ①-2 提交审批 → 走完（部门主管 2 票 + 财务经理 1 票）→ ACTIVE ====="
api POST "/business/erp/budget/$BUD/submit" "$ADMIN" > "$EV/raw/b2_submit.json"
echo "submit http=$HTTP $(jq -c '{code,msg,status:.data.budget.status}' "$EV/raw/b2_submit.json")"
BINST=$(psqlat "select id from biz_flow_instance where biz_type = 'BUDGET_APPROVE' and biz_id = '$BUD'::uuid order by created_at desc limit 1")
echo "budget_instance=$BINST"
[ -n "$BINST" ] || { echo "FATAL: 预算审批实例没起来"; exit 1; }
echo "$BINST" > "$EV/raw/budget_inst.id"
psqlq "select node_key, status, count(*) as tasks, sum(case when status='PENDING' then 1 else 0 end) as pending from biz_flow_task where instance_id = '$BINST'::uuid group by node_key, status order by node_key"
approve_instance "$BINST" "P0-06 预算编制同意"
sleep 2
psqlq "select budget_no, status, approved_at is not null as approved, effective_from from biz_budget where id = '$BUD'::uuid"

echo "===== ② 查预算余额（GET 详情 + DB 台账双侧）====="
api GET "/business/erp/budget/$BUD" "$ADMIN" > "$EV/raw/b3_detail.json"
echo "http=$HTTP"
jq -c '{status:.data.budget.status,total_amount:.data.budget.total_amount,year_used:.data.year_used,year_available:.data.year_available,p11:[.data.periods[]|select(.period_code=="2026-11")|{period_code,amount,used,available}],usages:(.data.usages|length)}' "$EV/raw/b3_detail.json"
echo "--- DB：biz_budget_usage 该槽行数 / 已用合计 ---"
psqlat "select count(*) || ' rows, used=' || coalesce(sum(amount) filter (where status in ('OCCUPIED','CONSUMED')),0) from biz_budget_usage where dept_id = '$DEPT'::uuid and account_id = '$ACC_TRAVEL'::uuid and fiscal_year = 2026 and deleted_at is null"
