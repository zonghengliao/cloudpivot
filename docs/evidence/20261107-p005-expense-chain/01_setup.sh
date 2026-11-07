#!/usr/bin/env bash
# ① 演练前置：部门 DRILL_EXP + 三个账号（直属上级 lead_drill / 财务经理 fin_drill / 报销人 emp_drill）
#    以及只给报销权限的专用角色 DRILL_CLERK（避免报销人自己被派成审批人）
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p005-expense-chain/00_env.sh

echo "===== 登录 admin ====="
ADMIN=$(login admin "$ADMIN_PASS")
echo "admin token len=${#ADMIN}"
[ -n "$ADMIN" ] || { echo "FATAL: admin 登录失败"; exit 1; }
echo "$ADMIN" > "$(tokf admin)"

echo "===== 1-1 部门 DRILL_EXP ====="
api POST "/system/dept" "$ADMIN" '{"dept_code":"DRILL_EXP","dept_name":"演练费用部","status":1,"sort_order":91}' > "$EV/raw/s1_dept.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/s1_dept.json")"
DEPT=$(jq -r '.data.id // empty' "$EV/raw/s1_dept.json")
if [ -z "$DEPT" ]; then
  api GET "/system/dept/tree" "$ADMIN" > "$EV/raw/s1_dept_tree.json"
  DEPT=$(jq -r '[..|objects|select(.dept_code?=="DRILL_EXP")][0].id // empty' "$EV/raw/s1_dept_tree.json")
fi
echo "dept_id=$DEPT"
[ -n "$DEPT" ] || { echo "FATAL: 部门没建起来"; exit 1; }
echo "$DEPT" > "$EV/raw/dept.id"

echo "===== 1-2 专用角色 DRILL_CLERK（只报销，不含 business:flow:approve）====="
api POST "/system/role" "$ADMIN" '{"role_code":"DRILL_CLERK","role_name":"演练报销员","description":"P0-05 演练：只能建/改/提交自己的报销单","status":1,"data_scope":4,"sort_order":91}' > "$EV/raw/s2_role.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/s2_role.json")"
ROLE=$(jq -r '.data.id // empty' "$EV/raw/s2_role.json")
if [ -z "$ROLE" ]; then
  api GET "/system/role/list" "$ADMIN" > "$EV/raw/s2_role_list.json"
  ROLE=$(jq -r '[.data[]?|select(.role_code=="DRILL_CLERK")][0].id // empty' "$EV/raw/s2_role_list.json")
fi
echo "role_id=$ROLE"
echo "$ROLE" > "$EV/raw/role.id"
[ -n "$ROLE" ] || { echo "FATAL: 角色没建起来"; exit 1; }

echo "===== 1-3 给 DRILL_CLERK 授权（费用类型查询 + 报销单增改查提交）====="
MENUS='["20000000-0000-0000-0000-000000001503","20000000-0000-0000-0000-000000001501","20000000-0000-0000-0000-000000001511","20000000-0000-0000-0000-000000001512","20000000-0000-0000-0000-000000001516","20000000-0000-0000-0000-000000001514"]'
api PUT "/system/role/$ROLE/menus" "$ADMIN" "{\"menu_ids\":$MENUS}" > "$EV/raw/s3_menus.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/s3_menus.json")"

echo "===== 1-4 建三个账号 ====="
mkuser() { # $1=username $2=real_name $3=role_ids_json
  api POST "/system/user" "$ADMIN" "{\"username\":\"$1\",\"password\":\"$DRILL_PWD\",\"real_name\":\"$2\",\"dept_id\":\"$DEPT\",\"status\":1,\"role_ids\":$3}" > "$EV/raw/s4_user_$1.json"
  echo "  $1 http=$HTTP $(jq -c '{code,msg}' "$EV/raw/s4_user_$1.json")"
  local id
  id=$(jq -r '.data.id // empty' "$EV/raw/s4_user_$1.json")
  if [ -z "$id" ]; then
    api GET "/system/user/page?page=1&page_size=100&keyword=$1" "$ADMIN" > "$EV/raw/s4_user_list_$1.json"
    id=$(jq -r --arg u "$1" '[.data.list[]?|select(.username==$u)][0].id // empty' "$EV/raw/s4_user_list_$1.json")
  fi
  echo "  $1 id=$id"
  printf '%s' "$id" > "$EV/raw/$1.id"
}
mkuser lead_drill "演练直属上级" '["20000000-0000-4000-8000-0000000f0001"]'
mkuser fin_drill  "演练财务经理" '["20000000-0000-4000-8000-0000000f0005"]'
mkuser emp_drill  "演练报销人"   "[\"$ROLE\"]"

echo "===== 1-5 部门负责人 = lead_drill（报销流「直属上级」靠它解析）====="
LEAD=$(cat "$EV/raw/lead_drill.id")
api PUT "/system/dept/$DEPT" "$ADMIN" "{\"dept_code\":\"DRILL_EXP\",\"dept_name\":\"演练费用部\",\"leader_id\":\"$LEAD\",\"status\":1,\"sort_order\":91}" > "$EV/raw/s5_dept_put.json"
echo "http=$HTTP $(jq -c '{code,msg,leader_id:.data.leader_id}' "$EV/raw/s5_dept_put.json")"

echo "===== 1-6 三个账号登录 ====="
for u in lead_drill fin_drill emp_drill; do
  T=$(login "$u" "$DRILL_PWD")
  echo "  $u token len=${#T}"
  echo "$T" > "$(tokf "$u")"
done

echo "===== 1-7 DB 复核 ====="
cat > /tmp/p005_setup_verify.sql <<SQL
\pset pager off
select id, dept_code, dept_name, leader_id from sys_dept where dept_code = 'DRILL_EXP';
select u.username, u.real_name, u.dept_id, r.role_code
  from sys_user u left join sys_user_role ur on ur.user_id = u.id left join sys_role r on r.id = ur.role_id
 where u.username in ('lead_drill','fin_drill','emp_drill') order by u.username;
select r.role_code, count(*) as menu_cnt
  from sys_role r join sys_role_menu rm on rm.role_id = r.id
 where r.role_code = 'DRILL_CLERK' group by r.role_code;
select m.auth_code from sys_role r join sys_role_menu rm on rm.role_id=r.id join sys_menu m on m.id=rm.menu_id
 where r.role_code='DRILL_CLERK' order by m.auth_code;
SQL
psqlf /tmp/p005_setup_verify.sql
