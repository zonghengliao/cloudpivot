#!/usr/bin/env bash
# ============================================================================
# FIX-1 验收 ①④：全新库（只有 admin）+ 演示账号种子**关闭**
#
# 场景：
#   1. admin 建销售订单 → 提交审批 → 422 / 65018（内控不许自审）
#      **但报文必须带结构化 data**（节点 / 角色 / 成员数 / 原因 / 修复指引）
#   2. GET /system/setup/check 报出缺人的关键角色
#   3. POST /system/setup/assign 指派一名非制单人 → 重试提交 → 200 SUBMITTED
#   4. 被指派的人能审批（待办 → approve → 实例 APPROVED）
#   5. 普通用户（只有 SALES_SUPERVISOR，没有 system:setup:manage）
#      → /system/setup/assign 403、/system/setup/check 403（不能自助指派）
#
# 运行位置：145 本机（实例监听 127.0.0.1:8035）
# ============================================================================
set -uo pipefail
BASE="http://127.0.0.1:8038"
API="$BASE/api"
EV="${EV:-/root/fix1/evidence-fresh2}"; mkdir -p "$EV"
HTTP=""
RUN="$(date +%Y%m%d%H%M%S)"

login() { # $1=username $2=password -> token（失败打印空）
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" |
    jq -r '.data.access_token // empty'
}

change_pwd() { # $1=token $2=old $3=new
  curl -sS -o /dev/null -w '%{http_code}' -X PUT "$API/system/auth/password" \
    -H "Authorization: Bearer $1" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$2\",\"new_password\":\"$3\"}"
}

api() { # $1=method $2=path $3=token $4=body
  local method="$1" path="$2" tok="$3" body="${4:-}"
  local out
  if [ -n "$body" ]; then
    out=$(curl -sS -o /tmp/_b.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    out=$(curl -sS -o /tmp/_b.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  HTTP="$out"
  cat /tmp/_b.json
}

step() { echo; echo "===== $* ====="; }

# ---------------------------------------------------------------- 0. 登录 + 改密
step "0. 登录 admin 并改密（首登强制改密，CODE-06）"
ADMIN_PWD_INIT=$(grep -m1 ^NIK_ADMIN_PASSWORD= /root/fix1/env5 | cut -d= -f2-)
ADMIN_PWD="Admin@Fix1a"
T=$(login admin "$ADMIN_PWD_INIT")
echo "login admin -> token_len=${#T}"
echo "change password http=$(change_pwd "$T" "$ADMIN_PWD_INIT" "$ADMIN_PWD")"
T=$(login admin "$ADMIN_PWD")
echo "relogin admin -> token_len=${#T}"

# ---------------------------------------------------------------- 1. 主数据
step "1. 客户 / 物料（admin）"
api POST /business/erp/customer "$T" \
  "{\"code\":\"FIX1-C$RUN\",\"name\":\"FIX1 演练客户\",\"status\":\"ACTIVE\"}" > "$EV/01_customer.json"
echo "customer http=$HTTP $(jq -c '{code,msg,id:.data.id}' "$EV/01_customer.json")"
CUST=$(jq -r '.data.id // empty' "$EV/01_customer.json")

UNIT=$(curl -sS "$API/business/erp/unit/page?page=1&page_size=10" -H "Authorization: Bearer $T" | jq -r '.data.list[0].id')
echo "unit=$UNIT"

api POST /business/erp/material "$T" \
  "{\"material_code\":\"FIX1-M$RUN\",\"material_name\":\"FIX1 演练物料\",\"spec\":\"FIX1\",\"unit_id\":\"$UNIT\",\"material_type\":\"FINISHED\",\"costing_method\":\"MOVING_AVG\",\"tax_rate\":0,\"safety_stock\":0,\"status\":1}" > "$EV/01_material.json"
echo "material http=$HTTP $(jq -c '{code,msg,id:.data.id}' "$EV/01_material.json")"
MAT=$(jq -r '.data.id // empty' "$EV/01_material.json")

# ---------------------------------------------------------------- 2. 自检（向导清单）
step "2. GET /system/setup/check —— 关键角色谁还没人"
api GET /system/setup/check "$T" > "$EV/02_check_before.json"
echo "http=$HTTP all_ready=$(jq -r '.data.all_ready' "$EV/02_check_before.json") demo_users_enabled=$(jq -r '.data.demo_users_enabled' "$EV/02_check_before.json")"
echo "missing=$(jq -r '[.data.missing[].role_code] | join(",")' "$EV/02_check_before.json")"
jq -c '.data.missing[] | {role_code, member_count, usable_member_count, why}' "$EV/02_check_before.json"

# ---------------------------------------------------------------- 3. 建单后提交 → 422 带 data
step "3. 建销售订单并提交（应 422 / 65018，且带结构化 data）"
api POST /business/erp/sales-order "$T" \
  "{\"customer_id\":\"$CUST\",\"items\":[{\"material_id\":\"$MAT\",\"qty\":10,\"price\":100}]}" > "$EV/03_so.json"
echo "create SO http=$HTTP so=$(jq -r '.data.so_no' "$EV/03_so.json") status=$(jq -r '.data.status' "$EV/03_so.json")"
SO=$(jq -r '.data.id' "$EV/03_so.json")

api POST "/business/erp/sales-order/$SO/submit-approval" "$T" '{}' > "$EV/04_submit_422.json"
echo "submit http=$HTTP code=$(jq -r '.code' "$EV/04_submit_422.json")"
echo "msg=$(jq -r '.msg' "$EV/04_submit_422.json")"
echo "----- 结构化 data（前端据此弹「指派审批人」）-----"
jq -c '.data' "$EV/04_submit_422.json"
echo "断言：$(jq -r 'if (.data.node_key and .data.role_code and .data.reason and .data.fix_hint) then "data 四要素齐全 ✔" else "缺字段 ✘" end' "$EV/04_submit_422.json")"
echo "reason=$(jq -r '.data.reason' "$EV/04_submit_422.json") role=$(jq -r '.data.role_code' "$EV/04_submit_422.json") member_count=$(jq -r '.data.member_count' "$EV/04_submit_422.json")"

# ---------------------------------------------------------------- 4. 建审批人 + 指派
step "4. 建一名非制单人 fix1_mgr_$RUN，并用 /system/setup/assign 指派 SALES_SUPERVISOR"
api POST /system/user "$T" \
  "{\"username\":\"fix1_mgr_$RUN\",\"password\":\"Fix1Mgr@2026\",\"real_name\":\"FIX1 销售主管\",\"status\":1,\"role_ids\":[]}" > "$EV/05_user.json"
echo "create user http=$HTTP $(jq -c '{code,msg}' "$EV/05_user.json")"
MGR_ID=$(curl -sS "$API/system/user/page?page=1&page_size=50&username=fix1_mgr_$RUN" -H "Authorization: Bearer $T" | jq -r '.data.list[0].id')
echo "mgr_id=$MGR_ID"

api POST /system/setup/assign "$T" "{\"role_code\":\"SALES_SUPERVISOR\",\"user_ids\":[\"$MGR_ID\"]}" > "$EV/06_assign.json"
echo "assign http=$HTTP $(jq -c '{code,code_:.data.role_code,added:.data.added,skipped:.data.skipped,member_count:.data.member_count}' "$EV/06_assign.json")"

step "4b. 重复指派（幂等：added=[] / skipped 含该用户）"
api POST /system/setup/assign "$T" "{\"role_code\":\"SALES_SUPERVISOR\",\"user_ids\":[\"$MGR_ID\"]}" > "$EV/07_assign_again.json"
echo "assign(2nd) http=$HTTP $(jq -c '{added:.data.added,skipped:.data.skipped,member_count:.data.member_count}' "$EV/07_assign_again.json")"

# ---------------------------------------------------------------- 5. 重新提交 → 成功
step "5. 重新提交审批（自动重试的等价操作）"
api POST "/business/erp/sales-order/$SO/submit-approval" "$T" '{}' > "$EV/08_submit_ok.json"
echo "submit http=$HTTP code=$(jq -r '.code' "$EV/08_submit_ok.json") status=$(jq -r '.data.status' "$EV/08_submit_ok.json") instance=$(jq -r '.data.approval_instance_id' "$EV/08_submit_ok.json")"
INST=$(jq -r '.data.approval_instance_id' "$EV/08_submit_ok.json")

# ---------------------------------------------------------------- 6. 待办 + 审批
step "6. fix1_mgr_$RUN 登录 → 改密 → 待办 → 审批"
MGR_INIT="Fix1Mgr@2026"
MT=$(login fix1_mgr_$RUN "$MGR_INIT")
echo "login mgr token_len=${#MT} must_change_password=$(curl -sS "$API/system/auth/me" -H "Authorization: Bearer $MT" | jq -r '.data.must_change_password')"
echo "change pwd http=$(change_pwd "$MT" "$MGR_INIT" "Fix1MgrA@2026")"
MT=$(login fix1_mgr_$RUN "Fix1MgrA@2026")

api GET "/business/flow/task/todo?page=1&page_size=10" "$MT" > "$EV/09_todo.json"
echo "todo http=$HTTP total=$(jq -r '.data.total' "$EV/09_todo.json")"
TASK=$(jq -r '.data.list[0].id // empty' "$EV/09_todo.json")
echo "task=$TASK node=$(jq -r '.data.list[0].node_name' "$EV/09_todo.json") assignee=$(jq -r '.data.list[0].assignee_name' "$EV/09_todo.json")"

api POST "/business/flow/task/$TASK/approve" "$MT" '{"comment":"FIX-1 验收：指派后审批通过"}' > "$EV/10_approve.json"
echo "approve http=$HTTP $(jq -c '{code,msg}' "$EV/10_approve.json")"

api GET "/business/erp/sales-order/$SO" "$T" > "$EV/11_so_after.json"
echo "SO status=$(jq -r '.data.status' "$EV/11_so_after.json")"
api GET "/business/flow/instance/$INST" "$T" > "$EV/12_instance.json"
echo "instance status=$(jq -r '.data.status' "$EV/12_instance.json")"

# ---------------------------------------------------------------- 7. 普通用户不能自助指派
step "7. 普通用户（fix1_mgr_$RUN，无 system:setup:manage）调 setup 接口 → 403"
api POST /system/setup/assign "$MT" "{\"role_code\":\"SALES_SUPERVISOR\",\"user_ids\":[\"$MGR_ID\"]}" > "$EV/13_forbidden_assign.json"
echo "assign as ordinary user http=$HTTP code=$(jq -r '.code' "$EV/13_forbidden_assign.json") msg=$(jq -r '.msg' "$EV/13_forbidden_assign.json")"
api GET /system/setup/check "$MT" > "$EV/14_forbidden_check.json"
echo "check as ordinary user http=$HTTP code=$(jq -r '.code' "$EV/14_forbidden_check.json")"

# ---------------------------------------------------------------- 8. 收尾自检
step "8. 再查一次 setup/check（SALES_SUPERVISOR 应已就绪，其余仍缺）"
api GET /system/setup/check "$T" > "$EV/15_check_after.json"
jq -c '{all_ready:.data.all_ready, missing:[.data.missing[].role_code], roles:[.data.roles[]|{role_code,ready,member_count}]}' "$EV/15_check_after.json"

echo
echo "===== 证据目录：$EV ====="
ls -1 "$EV" | head -30
