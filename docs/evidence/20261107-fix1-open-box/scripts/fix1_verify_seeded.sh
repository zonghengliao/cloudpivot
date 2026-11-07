#!/usr/bin/env bash
# ============================================================================
# FIX-1 验收 ②：全新库 + NIK_SEED_DEMO_USERS=true
#
# 场景：启动即有 9 个演示账号（卡面 8 个 + demo_gm），六条链的审批岗全部有人。
#   · demo_sales（销售员）建单 → 提交 → **直接 200 SUBMITTED**（不再 422）
#   · demo_sales_mgr（销售主管）待办里拿到这一票 → 审批 → 实例 APPROVED
#   · /system/setup/check → all_ready=true（6 个关键角色全就绪）
#
# 运行位置：145 本机（实例监听 127.0.0.1:8034 / 库 cloudpivot_fix1）
# ============================================================================
set -uo pipefail
BASE="http://127.0.0.1:8034"
API="$BASE/api"
EV="${EV:-/root/fix1/evidence-seeded}"; mkdir -p "$EV"
HTTP=""

login() {
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" |
    jq -r '.data.access_token // empty'
}
change_pwd() {
  curl -sS -o /dev/null -w '%{http_code}' -X PUT "$API/system/auth/password" \
    -H "Authorization: Bearer $1" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$2\",\"new_password\":\"$3\"}"
}
api() {
  local method="$1" path="$2" tok="$3" body="${4:-}"
  local out
  if [ -n "$body" ]; then
    out=$(curl -sS -o /tmp/_b2.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    out=$(curl -sS -o /tmp/_b2.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$out"; cat /tmp/_b2.json
}
step() { echo; echo "===== $* ====="; }

step "0. 启动日志里的种子结果（created=9 / existed=0 / dept_created=true）"
grep -o '演示账号种子完成.*' /root/fix1/run.log | head -1

step "1. admin 登录 + 改密"
ADMIN_INIT=$(grep -m1 ^NIK_ADMIN_PASSWORD= /root/fix1/env | cut -d= -f2-)
T=$(login admin "$ADMIN_INIT")
echo "login admin token_len=${#T}"
echo "change pwd http=$(change_pwd "$T" "$ADMIN_INIT" "Admin@Fix1b")"
T=$(login admin "Admin@Fix1b")

step "2. GET /system/setup/check —— 演示种子开 ⇒ 关键角色全就绪"
api GET /system/setup/check "$T" > "$EV/01_check.json"
echo "http=$HTTP all_ready=$(jq -r '.data.all_ready' "$EV/01_check.json") demo_users_enabled=$(jq -r '.data.demo_users_enabled' "$EV/01_check.json")"
jq -c '.data.roles[] | {role_code, role_name, member_count, usable_member_count, ready}' "$EV/01_check.json"

step "3. 主数据（admin）"
api POST /business/erp/customer "$T" \
  "{\"code\":\"SEED-C1\",\"name\":\"种子演练客户\",\"status\":\"ACTIVE\"}" > "$EV/02_customer.json"
CUST=$(jq -r '.data.id // empty' "$EV/02_customer.json")
echo "customer http=$HTTP id=$CUST"
UNIT=$(curl -sS "$API/business/erp/unit/page?page=1&page_size=10" -H "Authorization: Bearer $T" | jq -r '.data.list[0].id')
api POST /business/erp/material "$T" \
  "{\"material_code\":\"SEED-M1\",\"material_name\":\"种子演练物料\",\"spec\":\"SEED\",\"unit_id\":\"$UNIT\",\"material_type\":\"FINISHED\",\"costing_method\":\"MOVING_AVG\",\"tax_rate\":0,\"safety_stock\":0,\"status\":1}" > "$EV/03_material.json"
MAT=$(jq -r '.data.id // empty' "$EV/03_material.json")
echo "material http=$HTTP id=$MAT"

step "4. demo_sales 登录 + 改密（首次登录强制改密）"
# 可重跑：优先用「已改过的口令」，失败再退回初始口令（首跑时是后者）
DS=$(login demo_sales "Demo100@2026")
if [ -z "$DS" ]; then
  DS=$(login demo_sales "Fix1Demo@2026")
  echo "login demo_sales token_len=${#DS} must_change=$(curl -sS "$API/system/auth/me" -H "Authorization: Bearer $DS" | jq -r '.data.must_change_password')"
  echo "change pwd http=$(change_pwd "$DS" "Fix1Demo@2026" "Demo100@2026")"
  DS=$(login demo_sales "Demo100@2026")
fi
echo "demo_sales ready token_len=${#DS}"

step "5. demo_sales 建销售订单 → 提交审批（应直接 200，不再 65018）"
api POST /business/erp/sales-order "$DS" \
  "{\"customer_id\":\"$CUST\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":10,\"price\":100}]}" > "$EV/04_so.json"
echo "create SO http=$HTTP so=$(jq -r '.data.so_no' "$EV/04_so.json") status=$(jq -r '.data.status' "$EV/04_so.json") msg=$(jq -r '.msg' "$EV/04_so.json")"
SO=$(jq -r '.data.id // empty' "$EV/04_so.json")

api POST "/business/erp/sales-order/$SO/submit-approval" "$DS" '{}' > "$EV/05_submit.json"
echo "submit http=$HTTP code=$(jq -r '.code' "$EV/05_submit.json") status=$(jq -r '.data.status' "$EV/05_submit.json") instance=$(jq -r '.data.approval_instance_id' "$EV/05_submit.json") msg=$(jq -r '.msg' "$EV/05_submit.json")"
INST=$(jq -r '.data.approval_instance_id' "$EV/05_submit.json")

step "6. demo_sales_mgr 登录 + 改密 → 待办 → 审批"
DM=$(login demo_sales_mgr "DemoMgr100@2026")
if [ -z "$DM" ]; then
  DM=$(login demo_sales_mgr "Fix1Demo@2026")
  echo "change pwd http=$(change_pwd "$DM" "Fix1Demo@2026" "DemoMgr100@2026")"
  DM=$(login demo_sales_mgr "DemoMgr100@2026")
fi
api GET "/business/flow/task/todo?page=1&page_size=10" "$DM" > "$EV/06_todo.json"
echo "todo http=$HTTP total=$(jq -r '.data.total' "$EV/06_todo.json") node=$(jq -r '.data.list[0].node_name' "$EV/06_todo.json") assignee=$(jq -r '.data.list[0].assignee_name' "$EV/06_todo.json")"
TASK=$(jq -r '.data.list[0].id // empty' "$EV/06_todo.json")
api POST "/business/flow/task/$TASK/approve" "$DM" '{"comment":"FIX-1 验收②：演示账号直接跑通"}' > "$EV/07_approve.json"
echo "approve http=$HTTP code=$(jq -r '.code' "$EV/07_approve.json") msg=$(jq -r '.msg' "$EV/07_approve.json")"

api GET "/business/erp/sales-order/$SO" "$T" > "$EV/08_so_after.json"
echo "SO status=$(jq -r '.data.status' "$EV/08_so_after.json")"
api GET "/business/flow/instance/$INST" "$T" > "$EV/09_instance.json"
echo "instance status=$(jq -r '.data.status' "$EV/09_instance.json")"

echo
echo "===== 证据目录：$EV ====="
ls -1 "$EV"
