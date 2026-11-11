#!/usr/bin/env bash
# =============================================================================
# HR-15 HTTP 冒烟：劳动合同 3 接口 + 续签链 + 到期提醒字段
# =============================================================================
#
# 为什么还要有它（真库用例已经证明了续签链与状态机）
# ----------------------------------------------
# `cargo test` 走 handler 里的**业务函数**，不经过 axum 路由、权限宏、JSON 反序列化。
# 这几件事只有起真实例才验得到：
#
#   1. 三条路由真的注册上了（`POST /employee/{id}/contract` 不会被 `/employee/{id}` 抢走）；
#   2. 未登录是 401（不是「合同无人管」）；
#   3. 权限码 `business:hr:contract:manage` 真能过（V0387 种子 + 超管短路）；
#   4. JSON 字段名与前端 `api/business/hr/contract.ts` 对得上（`to_status` / `terminated_date` …）；
#   5. 员工详情的 `contracts[]` 在两次请求之间真的长出了新合同（前端时间线就靠它）。
#
# 用法（隔离实例）：
#   BASE_URL=http://127.0.0.1:18097 ADMIN_PASSWORD='Admin@123456' \
#     bash scripts/verify/hr/contract_http_smoke.sh
#
# 环境变量
#   BASE_URL        后端根地址（默认 http://127.0.0.1:18097）
#   ADMIN_PASSWORD  超管密码（**没有默认值**：仓库不留默认口令）
#   KEEP=1          结束后不软删冒烟员工（留着排查）
#
# 注意（空库首启）：`admin` 的 `must_change_password=true`，脚本会先改密再重登，
# 新口令是 `${ADMIN_PASSWORD}-changed` —— **第二次**跑就要传新口令。
# =============================================================================
set -uo pipefail

BASE_URL=${BASE_URL:-http://127.0.0.1:18097}
ADMIN_PASSWORD=${ADMIN_PASSWORD:-}
KEEP=${KEEP:-0}
TAG=$(date +%H%M%S%N | cut -c1-12)
OUT=$(mktemp)
CODE=0
TOKEN=""
pass=0
fail=0

TODAY=$(date +%F)
START=$(date -d '-365 days' +%F)
SIGN=$(date -d '-400 days' +%F)
END30=$(date -d '+30 days' +%F)
END365=$(date -d '+365 days' +%F)
END400=$(date -d '+400 days' +%F)
END730=$(date -d '+730 days' +%F)

pass_line() { printf '  [PASS] %s\n' "$1"; pass=$((pass + 1)); }
fail_line() { printf '  [FAIL] %s\n' "$1"; fail=$((fail + 1)); }

expect_eq() { # name expect actual
  if [ "$2" = "$3" ]; then pass_line "$1（$3）"; else fail_line "$1：期望 $2，实际 $3"; fi
}

expect_contains() { # name haystack needle
  case "$2" in
    *"$3"*) pass_line "$1（含 $3）" ;;
    *) fail_line "$1：期望包含 $3，实际 ${2:0:300}" ;;
  esac
}

expect_absent() { # name haystack needle
  case "$2" in
    *"$3"*) fail_line "$1：出现了不该出现的 $3" ;;
    *) pass_line "$1（不含 $3）" ;;
  esac
}

req() { # METHOD PATH [BODY]
  local method=$1 path=$2 body=${3:-}
  local args=(-s -o "$OUT" -w '%{http_code}' -X "$method" "$BASE_URL$path" -H 'Content-Type: application/json')
  [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
  [ -n "$body" ] && args+=(-d "$body")
  CODE=$(curl "${args[@]}")
}

login() {
  local user=$1 password=$2
  req POST /api/system/auth/login "{\"username\":\"$user\",\"password\":\"$password\"}"
  local must
  must=$(jq -r '.data.must_change_password // false' "$OUT" 2>/dev/null)
  TOKEN=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
  [ -n "$TOKEN" ] || return 1
  if [ "$must" = "true" ]; then
    req PUT /api/system/auth/password \
      "{\"old_password\":\"$password\",\"new_password\":\"${password}-changed\"}"
    password="${password}-changed"
    TOKEN=""
    req POST /api/system/auth/login "{\"username\":\"$user\",\"password\":\"$password\"}"
    TOKEN=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
    [ -n "$TOKEN" ] || return 1
  fi
  return 0
}

echo "== HR-15 劳动合同 HTTP 冒烟（$BASE_URL，tag=$TAG）=="

# ---- 0. 前置 ----
if [ -z "$ADMIN_PASSWORD" ]; then
  echo "FATAL: 必须显式给 ADMIN_PASSWORD（仓库不留默认口令）" >&2
  exit 2
fi
CODE=$(curl -s -o /dev/null -w '%{http_code}' -m 5 "$BASE_URL/health")
expect_eq "0. /health 就绪" 200 "$CODE"
if ! login admin "$ADMIN_PASSWORD"; then
  echo "FATAL: admin 登录失败（检查 ADMIN_PASSWORD / 实例库）" >&2
  exit 2
fi
pass_line "0. admin 登录（含必改密处理）"

# ---- 1. 造一个员工（合同要挂在真实员工上）----
req GET /api/system/dept/tree
DEPT_ID=$(jq -r '.data[0].id // empty' "$OUT")
if [ -z "$DEPT_ID" ]; then
  echo "FATAL: 库里没有部门，无法建员工" >&2
  exit 2
fi
req POST /api/business/hr/employee "{
  \"name\":\"合同冒烟员工$TAG\",\"gender\":1,\"dept_id\":\"$DEPT_ID\",
  \"hire_date\":\"2024-03-01\",\"mobile\":\"13800000021\",\"id_card\":\"110101$TAG\",
  \"remark\":\"HR-15 冒烟夹具 $TAG\"}"
EMP_ID=$(jq -r '.data.id // empty' "$OUT")
expect_eq "1. 新建冒烟员工 = 200" 200 "$CODE"
if [ -z "$EMP_ID" ]; then
  echo "FATAL: 员工没建成：$(jq -c '.msg' "$OUT")" >&2
  exit 2
fi
pass_line "1. 员工 id=$EMP_ID"

NO1="HT15-SMOKE-$TAG-A"
NO2="HT15-SMOKE-$TAG-B"
NO3="HT15-SMOKE-$TAG-C"

# ---- 2. 未登录 → 401 ----
saved=$TOKEN
TOKEN=""
req POST "/api/business/hr/employee/$EMP_ID/contract" "{\"contract_no\":\"$NO1\"}"
expect_eq "2. 未登录登记合同 = 401" 401 "$CODE"
TOKEN=$saved

# ---- 3. 登记合同（终止日 = 今天 + 30 天）----
req POST "/api/business/hr/employee/$EMP_ID/contract" "{
  \"contract_no\":\"$NO1\",\"contract_type\":\"FIXED_TERM\",\"sign_date\":\"$SIGN\",
  \"start_date\":\"$START\",\"end_date\":\"$END30\",\"probation_months\":3,\"remark\":\"HR-15 冒烟\"}"
C1=$(jq -r '.data.id // empty' "$OUT")
expect_eq "3. #13 登记合同 = 200" 200 "$CODE"
expect_eq "3. 编号原样（trim 后）" "$NO1" "$(jq -r '.data.contract_no // empty' "$OUT")"
expect_eq "3. 初始状态 ACTIVE" "ACTIVE" "$(jq -r '.data.status // empty' "$OUT")"
expect_eq "3. 状态中文 = 生效中" "生效中" "$(jq -r '.data.status_label // empty' "$OUT")"
expect_eq "3. 到期提醒 = 30 天（end_date - today）" 30 "$(jq -r '.data.expire_in_days // empty' "$OUT")"
expect_eq "3. 首份合同没有续签来源" "null" "$(jq -r '.data.renewed_from_id // "null"' "$OUT")"
if [ -z "$C1" ]; then
  echo "FATAL: 合同没建成：$(jq -c '.msg' "$OUT")" >&2
  exit 2
fi
pass_line "3. 合同 id=$C1"

# ---- 4. 员工详情能看到这份合同（时间线的数据源）----
req GET "/api/business/hr/employee/$EMP_ID"
expect_eq "4. 详情 contracts[] 长度 = 1" 1 "$(jq -r '.data.contracts | length' "$OUT")"
expect_eq "4. current_contract = 刚建的那份" "$C1" "$(jq -r '.data.current_contract.id // empty' "$OUT")"
expect_eq "4. 详情里到期天数也是 30" 30 "$(jq -r '.data.current_contract.expire_in_days // empty' "$OUT")"

# ---- 5. 改合同（试用期 6 个月 + 清空备注）----
req PUT "/api/business/hr/contract/$C1" '{"probation_months":6,"remark":""}'
expect_eq "5. #14 修改合同 = 200" 200 "$CODE"
expect_eq "5. 试用期改成 6" 6 "$(jq -r '.data.probation_months // empty' "$OUT")"
expect_eq "5. 备注被清空" "null" "$(jq -r '.data.remark // "null"' "$OUT")"
expect_eq "5. 编号不受影响" "$NO1" "$(jq -r '.data.contract_no // empty' "$OUT")"

# ---- 6. 撞号 → 409 + 68005 ----
req POST "/api/business/hr/employee/$EMP_ID/contract" "{
  \"contract_no\":\"$NO1\",\"sign_date\":\"$SIGN\",\"start_date\":\"$START\",\"end_date\":\"$END30\"}"
expect_eq "6. 同编号再登记 = 409" 409 "$CODE"
expect_eq "6. 错误码 = 68005（合同编号冲突）" 68005 "$(jq -r '.code // 0' "$OUT")"
expect_contains "6. 报文点名编号" "$(jq -r '.msg // empty' "$OUT")" "$NO1"

# ---- 7. 缺编号 / 签订日晚于起始日 → 400（不是 500）----
req POST "/api/business/hr/employee/$EMP_ID/contract" "{\"sign_date\":\"$SIGN\",\"start_date\":\"$START\",\"end_date\":\"$END30\"}"
expect_eq "7. 缺 contract_no = 400" 400 "$CODE"
expect_contains "7. 报文点 contract_no" "$(jq -r '.msg // empty' "$OUT")" "contract_no"
req POST "/api/business/hr/employee/$EMP_ID/contract" "{
  \"contract_no\":\"HT15-BAD-$TAG\",\"sign_date\":\"$TODAY\",\"start_date\":\"$START\",\"end_date\":\"$END30\"}"
expect_eq "7. 签订日晚于起始日 = 400" 400 "$CODE"
expect_absent "7. 不泄内部细节" "$(cat "$OUT")" "panicked"

# ---- 8. 续签（#15）：新合同接链 + 旧合同封 RENEWED ----
req POST "/api/business/hr/contract/$C1/renew" "{
  \"contract_no\":\"$NO2\",\"start_date\":\"$END30\",\"end_date\":\"$END730\"}"
C2=$(jq -r '.data.contract.id // empty' "$OUT")
expect_eq "8. #15 续签 = 200" 200 "$CODE"
expect_eq "8. 原合同状态 = RENEWED" "RENEWED" "$(jq -r '.data.previous.status // empty' "$OUT")"
expect_eq "8. 原合同中文 = 已续签" "已续签" "$(jq -r '.data.previous.status_label // empty' "$OUT")"
expect_eq "8. 新合同指回原合同" "$C1" "$(jq -r '.data.contract.renewed_from_id // empty' "$OUT")"
expect_eq "8. 新合同带原合同编号" "$NO1" "$(jq -r '.data.contract.renewed_from_no // empty' "$OUT")"
expect_eq "8. 续签链长度 = 2" 2 "$(jq -r '.data.chain | length' "$OUT")"
expect_eq "8. 链首 = 原合同（可追溯到原合同）" "$C1" "$(jq -r '.data.chain[0].id // empty' "$OUT")"
expect_eq "8. 链尾 = 新合同" "$C2" "$(jq -r '.data.chain[1].id // empty' "$OUT")"
expect_eq "8. 新合同是 ACTIVE" "ACTIVE" "$(jq -r '.data.contract.status // empty' "$OUT")"

# ---- 9. 员工详情：两份历史、新的在前、现行合同 = 新合同 ----
req GET "/api/business/hr/employee/$EMP_ID"
expect_eq "9. 详情 contracts[] 长度 = 2" 2 "$(jq -r '.data.contracts | length' "$OUT")"
expect_eq "9. 台账新的在前（第一行是新合同）" "$C2" "$(jq -r '.data.contracts[0].id // empty' "$OUT")"
expect_eq "9. 旧合同在第二行且已续签" "RENEWED" "$(jq -r '.data.contracts[1].status // empty' "$OUT")"
expect_eq "9. 旧合同行带新合同编号？不：旧行来源为空" "null" "$(jq -r '.data.contracts[1].renewed_from_id // "null"' "$OUT")"
expect_eq "9. current_contract = 新合同" "$C2" "$(jq -r '.data.current_contract.id // empty' "$OUT")"
expect_eq "9. 已续签的旧合同不再报到期天数" "null" "$(jq -r '.data.contracts[1].expire_in_days // "null"' "$OUT")"

# ---- 10. 对已续签的原合同再续 → 409 + 68006 ----
req POST "/api/business/hr/contract/$C1/renew" "{
  \"contract_no\":\"$NO3\",\"start_date\":\"$END30\",\"end_date\":\"$END400\"}"
expect_eq "10. 对已续签合同再续 = 409" 409 "$CODE"
expect_eq "10. 错误码 = 68006（终态拦截）" 68006 "$(jq -r '.code // 0' "$OUT")"
expect_contains "10. 报文给下一步（续签链）" "$(jq -r '.msg // empty' "$OUT")" "续签"

# ---- 11. 解除：先缺日期 400，再补齐 200，然后字段锁定 409 ----
req PUT "/api/business/hr/contract/$C2" '{"to_status":"TERMINATED","terminate_reason":"协商一致解除劳动合同"}'
expect_eq "11. 解除缺 terminated_date = 400" 400 "$CODE"
expect_contains "11. 报文点 terminated_date" "$(jq -r '.msg // empty' "$OUT")" "terminated_date"
req PUT "/api/business/hr/contract/$C2" "{
  \"to_status\":\"TERMINATED\",\"terminated_date\":\"$END365\",\"terminate_reason\":\"协商一致解除劳动合同\"}"
expect_eq "11. 解除（带日期+原因）= 200" 200 "$CODE"
expect_eq "11. 状态 = TERMINATED" "TERMINATED" "$(jq -r '.data.status // empty' "$OUT")"
expect_eq "11. 解除日落库" "$END365" "$(jq -r '.data.terminated_date // empty' "$OUT")"
expect_eq "11. 解除后不再报到期天数" "null" "$(jq -r '.data.expire_in_days // "null"' "$OUT")"
req PUT "/api/business/hr/contract/$C2" '{"remark":"想偷偷改"}'
expect_eq "11. 已解除改字段 = 409" 409 "$CODE"
expect_eq "11. 错误码 = 68006" 68006 "$(jq -r '.code // 0' "$OUT")"

# ---- 12. 无改动的 PUT → 409（不写空日志）----
req PUT "/api/business/hr/contract/$C1" '{}'
expect_eq "12. 空 PUT = 409（没有改动就不写库）" 409 "$CODE"

# ---- 13. 收尾 ----
if [ "$KEEP" = "1" ]; then
  echo "  [SKIP] KEEP=1：保留冒烟员工 $EMP_ID（合同 $C1 / $C2）"
else
  req DELETE "/api/business/hr/employee/$EMP_ID"
  expect_eq "13. 软删冒烟员工 = 200" 200 "$CODE"
fi

echo "== 结果：PASS=$pass FAIL=$fail =="
[ "$fail" -eq 0 ]
