#!/usr/bin/env bash
# ① 演练前置：新建预算演练部门 DRILL_BDG（负责人 lead_drill），并登录全部审批相关账号。
#    复用 P0-05 已建的 lead_drill / fin_drill / emp_drill / demo_lead / gm_drill。
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh

export DRILL_PWD='Drill@123456'

echo "===== 1-1 登录 admin ====="
ADMIN=$(login admin "$ADMIN_PASS")
echo "admin token len=${#ADMIN}"
[ -n "$ADMIN" ] || { echo "FATAL: admin 登录失败"; exit 1; }
echo "$ADMIN" > "$(tokf admin)"

echo "===== 1-2 部门 DRILL_BDG ====="
api POST "/system/dept" "$ADMIN" '{"dept_code":"DRILL_BDG","dept_name":"演练预算部","status":1,"sort_order":92}' > "$EV/raw/s1_dept.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/s1_dept.json")"
DEPT=$(jq -r '.data.id // empty' "$EV/raw/s1_dept.json")
if [ -z "$DEPT" ]; then
  api GET "/system/dept/tree" "$ADMIN" > "$EV/raw/s1_dept_tree.json"
  DEPT=$(jq -r '[..|objects|select(.dept_code?=="DRILL_BDG")][0].id // empty' "$EV/raw/s1_dept_tree.json")
fi
echo "dept_id=$DEPT"
[ -n "$DEPT" ] || { echo "FATAL: 部门没建起来"; exit 1; }
echo "$DEPT" > "$EV/raw/dept.id"

echo "===== 1-3 部门负责人 = lead_drill ====="
LEADID=$(psqlat "select id from sys_user where username = 'lead_drill' and deleted_at is null")
FINID=$(psqlat "select id from sys_user where username = 'fin_drill' and deleted_at is null")
EMPID=$(psqlat "select id from sys_user where username = 'emp_drill' and deleted_at is null")
echo "lead=$LEADID fin=$FINID emp=$EMPID"
printf '%s' "$LEADID" > "$EV/raw/lead_drill.id"
printf '%s' "$FINID" > "$EV/raw/fin_drill.id"
printf '%s' "$EMPID" > "$EV/raw/emp_drill.id"
api PUT "/system/dept/$DEPT" "$ADMIN" "{\"dept_code\":\"DRILL_BDG\",\"dept_name\":\"演练预算部\",\"leader_id\":\"$LEADID\",\"status\":1,\"sort_order\":92}" > "$EV/raw/s2_dept_put.json"
echo "http=$HTTP $(jq -c '{code,msg,leader_id:.data.leader_id}' "$EV/raw/s2_dept_put.json")"

echo "===== 1-4 登录演练账号 ====="
try_login() { # $1=username  [$2..]=候选口令
  local u="$1"; shift
  local p t
  for p in "$@"; do
    t=$(login "$u" "$p")
    if [ -n "$t" ]; then echo "$t" > "$(tokf "$u")"; echo "  $u ok (pass=$p) len=${#t}"; return 0; fi
  done
  echo "  $u FAILED"
  return 1
}
try_login lead_drill "$DRILL_PWD"
try_login fin_drill  "$DRILL_PWD"
try_login emp_drill  "$DRILL_PWD"
try_login demo_lead  'Demo@123456' "$DRILL_PWD"
try_login gm_drill   "$DRILL_PWD" 'Demo@123456'

echo "===== 1-5 角色成员复核（预算审批：DEPT_LEADER 全员 + FINANCE_MANAGER）====="
cat > /tmp/p006_role.sql <<SQL
\pset pager off
select r.role_code, u.username from sys_role r
  join sys_user_role ur on ur.role_id = r.id join sys_user u on u.id = ur.user_id
 where r.role_code in ('DEPT_LEADER','FINANCE_MANAGER') order by r.role_code, u.username;
select id, dept_code, dept_name, leader_id from sys_dept where dept_code = 'DRILL_BDG';
SQL
psqlf /tmp/p006_role.sql

echo "===== 1-6 确认 lead_drill 是刚建部门的负责人（审批「直属上级」靠它）====="
psqlat "select leader_id from sys_dept where id = '$DEPT'::uuid"
