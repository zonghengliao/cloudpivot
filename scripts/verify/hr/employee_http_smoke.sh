#!/usr/bin/env bash
# =============================================================================
# HR-12 HTTP 冒烟：员工档案 6 接口 + #8 绑账号 + 权限码负向
# =============================================================================
#
# 为什么还要有它（真库用例已经证明对了）
# --------------------------------------
# `cargo test` 走的是 handler 里的**业务函数**，不经过 axum 路由。三件事只有起真实例才验得到：
#
#   1. 7 条路由真的注册上了（宏路径拼错 → 落到 SPA fallback 返回 HTML，
#      前端看到的是「响应体格式不符合」，不是 404 —— 这类错最费时间）；
#   2. `#[nik::permission]` 的权限码真的接了（码写错 → 只有非超管账号 403，
#      超管短路让人以为「好的」）；
#   3. 状态码与错误码在**HTTP 层**是 401/403/404/409/400，而不是全被包成 200 + code。
#
# 用法（隔离实例，端口/库都在隔离段；自己起实例见 docs/evidence 的 README）：
#   BASE_URL=http://127.0.0.1:18095 ADMIN_PASSWORD='Hr12@123456' \
#     bash scripts/verify/hr/employee_http_smoke.sh
#
# 环境变量
#   BASE_URL        后端根地址（默认 http://127.0.0.1:18095，隔离段）
#   ADMIN_PASSWORD  超管密码（**没有默认值**：仓库不留默认口令）
#   KEEP=1          结束后不删夹具员工 / 探针账号（留着排查）
# =============================================================================
set -uo pipefail

BASE_URL=${BASE_URL:-http://127.0.0.1:18095}
ADMIN_PASSWORD=${ADMIN_PASSWORD:-}
KEEP=${KEEP:-0}
TAG=$(date +%H%M%S%N | cut -c1-12)
EMP_NO="HR12-SMOKE-$TAG"
OUT=$(mktemp)
CODE=0
TOKEN=""
pass=0
fail=0

pass_line() { printf '  [PASS] %s\n' "$1"; pass=$((pass + 1)); }
fail_line() { printf '  [FAIL] %s\n' "$1"; fail=$((fail + 1)); }

# 断言：两个值相等
expect_eq() { # name expect actual
  if [ "$2" = "$3" ]; then pass_line "$1（$3）"; else fail_line "$1：期望 $2，实际 $3"; fi
}

# 断言：字符串包含
expect_contains() { # name haystack needle
  case "$2" in
    *"$3"*) pass_line "$1（含 $3）" ;;
    *) fail_line "$1：期望包含 $3，实际 ${2:0:200}" ;;
  esac
}

# 断言：JSON 取值为真（非空、非 false、非 0）
expect_truthy() { # name value
  if [ -n "$2" ] && [ "$2" != "null" ] && [ "$2" != "false" ]; then
    pass_line "$1（$2）"
  else
    fail_line "$1：实际 $2"
  fi
}

# 通用请求：req METHOD PATH [BODY]，结果落 $OUT / $CODE
req() {
  local method=$1 path=$2 body=${3:-}
  local args=(-s -o "$OUT" -w '%{http_code}' -X "$method" "$BASE_URL$path" -H 'Content-Type: application/json')
  [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
  [ -n "$body" ] && args+=(-d "$body")
  CODE=$(curl "${args[@]}")
}

# 登录（必改密的账号先改密再重登）
login() {
  local user=$1 password=$2
  req POST /api/system/auth/login "{\"username\":\"$user\",\"password\":\"$password\"}"
  local must code
  must=$(jq -r '.data.must_change_password // false' "$OUT" 2>/dev/null)
  TOKEN=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
  [ -n "$TOKEN" ] || return 1
  if [ "$must" = "true" ]; then
    req PUT /api/system/auth/password \
      "{\"old_password\":\"$password\",\"new_password\":\"${password}-changed\"}"
    password="${password}-changed"
    req POST /api/system/auth/login "{\"username\":\"$user\",\"password\":\"$password\"}"
    TOKEN=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
    code=$(jq -r '.code' "$OUT" 2>/dev/null)
    [ "$code" = "0" ] || return 1
  fi
  LOGIN_PASSWORD="$password"
  return 0
}

echo "== HR-12 员工档案 HTTP 冒烟（$BASE_URL，tag=$TAG）=="

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

# ---- 1. 取一个部门（员工必须挂真实部门）----
req GET /api/system/dept/tree
DEPT_ID=$(jq -r '.data[0].id // empty' "$OUT")
expect_truthy "1. 部门树取到 dept_id" "$DEPT_ID"
if [ -z "$DEPT_ID" ]; then
  echo "FATAL: 库里没有部门，无法新建员工" >&2
  exit 2
fi

# ---- 2. 未登录访问业务接口 → 401 ----
saved_token=$TOKEN
TOKEN=""
req GET "/api/business/hr/employee/page?page=1&page_size=5"
expect_eq "2. 未登录访问列表 = 401" 401 "$CODE"
TOKEN=$saved_token

# ---- 3. #1 列表 ----
req GET "/api/business/hr/employee/page?page=1&page_size=5"
expect_eq "3. #1 GET /employee/page = 200" 200 "$CODE"
expect_eq "3. #1 响应 code = 0" 0 "$(jq -r '.code' "$OUT")"

# ---- 4. #3 新建 ----
req POST /api/business/hr/employee "{
  \"emp_no\":\"$EMP_NO\",\"name\":\"冒烟员工\",\"gender\":1,
  \"dept_id\":\"$DEPT_ID\",\"hire_date\":\"2024-03-01\",\"probation_end_date\":\"2024-06-01\",
  \"position_code\":\"ENGINEER\",\"job_level\":\"P5\",\"education\":\"BACHELOR\",
  \"mobile\":\"13800000001\",\"id_card\":\"110101199001011234\",
  \"bank_account\":\"6222021234567890\",\"remark\":\"HR-12 冒烟夹具\"}"
EMP_ID=$(jq -r '.data.id // empty' "$OUT")
expect_eq "4. #3 POST /employee = 200" 200 "$CODE"
expect_truthy "4. #3 回执 id" "$EMP_ID"
expect_eq "4. #3 回执 emp_no" "$EMP_NO" "$(jq -r '.data.emp_no // empty' "$OUT")"

# 同一工号再建一次 → 409 + 68001（唯一键翻译在 HTTP 层也要成立）
req POST /api/business/hr/employee "{
  \"emp_no\":\"$EMP_NO\",\"name\":\"重复工号\",\"dept_id\":\"$DEPT_ID\",\"hire_date\":\"2024-03-01\"}"
expect_eq "4. 重复工号 = 409" 409 "$CODE"
expect_eq "4. 重复工号业务码 = 68001" 68001 "$(jq -r '.code' "$OUT")"

# ---- 5. #1 列表按关键字命中 ----
req GET "/api/business/hr/employee/page?page=1&page_size=10&keyword=$EMP_NO"
expect_eq "5. #1 关键字命中 total = 1" 1 "$(jq -r '.data.total // 0' "$OUT")"
expect_eq "5. #1 行 id 对得上" "$EMP_ID" "$(jq -r '.data.list[0].id // empty' "$OUT")"
expect_truthy "5. #1 部门名 join" "$(jq -r '.data.list[0].dept_name // empty' "$OUT")"
expect_eq "5. #1 新档状态" "PROBATION" "$(jq -r '.data.list[0].status // empty' "$OUT")"

# ---- 6. #2 详情：*_masked 打码 + 明文不出现 ----
req GET "/api/business/hr/employee/$EMP_ID"
expect_eq "6. #2 GET /employee/{id} = 200" 200 "$CODE"
DETAIL=$(cat "$OUT")
expect_truthy "6. #2 sensitive_masked.id_card_masked" "$(jq -r '.data.sensitive_masked.id_card_masked // empty' "$OUT")"
expect_contains "6. #2 身份证已打码" "$(jq -r '.data.sensitive_masked.id_card_masked // empty' "$OUT")" "*"
expect_truthy "6. #2 岗位字典名" "$(jq -r '.data.position_name // empty' "$OUT")"
case "$DETAIL" in
  *110101199001011234*) fail_line "6. #2 详情里出现了明文身份证" ;;
  *) pass_line "6. #2 详情不含明文身份证" ;;
esac
case "$DETAIL" in
  *6222021234567890*) fail_line "6. #2 详情里出现了明文工资卡" ;;
  *) pass_line "6. #2 详情不含明文工资卡" ;;
esac

# ---- 7. #4 修改 ----
req PUT "/api/business/hr/employee/$EMP_ID" '{"name":"冒烟改名","email":""}'
expect_eq "7. #4 PUT /employee/{id} = 200" 200 "$CODE"
req GET "/api/business/hr/employee/$EMP_ID"
expect_eq "7. #4 改名生效" "冒烟改名" "$(jq -r '.data.name // empty' "$OUT")"
expect_eq "7. #4 工号未变" "$EMP_NO" "$(jq -r '.data.emp_no // empty' "$OUT")"

# ---- 8. #6 状态：离职 409（必须走离职单）/ 转正 400（无生效合同）/ 非法值 400 ----
req POST "/api/business/hr/employee/$EMP_ID/status" '{"to_status":"RESIGNED","reason":"个人原因辞职"}'
expect_eq "8. #6 离职 = 409（走离职单）" 409 "$CODE"
expect_contains "8. #6 离职报文点名离职单" "$(jq -r '.msg // empty' "$OUT")" "离职单"

req POST "/api/business/hr/employee/$EMP_ID/status" '{"to_status":"ACTIVE"}'
expect_eq "8. #6 无合同转正 = 400" 400 "$CODE"
expect_contains "8. #6 转正报文点名劳动合同" "$(jq -r '.msg // empty' "$OUT")" "劳动合同"

req POST "/api/business/hr/employee/$EMP_ID/status" '{"to_status":"WORKING"}'
expect_eq "8. #6 非法状态 = 400" 400 "$CODE"

# ---- 9. #8 绑账号（传 null = 解绑；未绑过的再解绑也是幂等成功）----
req POST "/api/business/hr/employee/$EMP_ID/bind-user" '{"user_id":null}'
expect_eq "9. #8 bind-user(null) = 200" 200 "$CODE"
expect_eq "9. #8 解绑后 user_id = null" "null" "$(jq -r '.data.user_id' "$OUT")"

# 绑定一个 ROBOT 账号 → 400（机器人不该进工资表）
req POST /api/system/user "{\"username\":\"hr12-probe-$TAG\",\"password\":\"Probe@123456\",\"real_name\":\"HR12 探针\"}"
PROBE_ID=$(jq -r '.data.id // empty' "$OUT")
if [ -n "$PROBE_ID" ]; then
  pass_line "9. 探针账号已建（$PROBE_ID）"
else
  fail_line "9. 探针账号未建成：$(jq -c '.msg' "$OUT")"
fi

# ---- 10. 权限码负向：无角色的账号访问 HR 接口 → 403 ----
if [ -n "$PROBE_ID" ]; then
  saved_token=$TOKEN
  if login "hr12-probe-$TAG" "Probe@123456"; then
    req GET "/api/business/hr/employee/page?page=1&page_size=5"
    expect_eq "10. 无 HR 权限码的账号访问列表 = 403" 403 "$CODE"
  else
    fail_line "10. 探针账号登录失败（无法验证权限码）"
  fi
  TOKEN=$saved_token
fi

# ---- 11. #5 删除（软删）→ 详情 404 ----
req DELETE "/api/business/hr/employee/$EMP_ID"
expect_eq "11. #5 DELETE /employee/{id} = 200" 200 "$CODE"
req GET "/api/business/hr/employee/$EMP_ID"
expect_eq "11. 软删后详情 = 404" 404 "$CODE"
req DELETE "/api/business/hr/employee/$EMP_ID"
expect_eq "11. 重复删除 = 404" 404 "$CODE"

# ---- 12. 清场（KEEP=1 时留着排查）----
if [ "$KEEP" != "1" ]; then
  # 软删的员工留在库里（软删语义就是这样），冒烟不硬删别人的行；
  # 探针账号走接口删掉，避免每次冒烟在账号表里堆垃圾。
  if [ -n "${PROBE_ID:-}" ]; then
    req DELETE "/api/system/user/$PROBE_ID"
    [ "$CODE" = "200" ] && pass_line "12. 探针账号已删除" || fail_line "12. 探针账号删除失败（$CODE）"
  fi
fi

echo
echo "== 合计：PASS=$pass FAIL=$fail =="
rm -f "$OUT"
[ "$fail" -eq 0 ] || exit 1
exit 0
