#!/usr/bin/env bash
# =============================================================================
# HR-13 HTTP 冒烟：敏感字段明文接口 + audit_logs 留痕 + sys_field_mask 打码
# =============================================================================
#
# 为什么还要有它（真库用例已经证明对了）
# --------------------------------------
# `cargo test` 走的是 handler 里的**业务函数**，不经过 axum 路由与权限宏。四件事只有起真实例才验得到：
#
#   1. `GET /business/hr/employee/{id}/sensitive` 真的注册上了（宏路径拼错 → 落 SPA fallback
#      返回 HTML，前端看到「响应体格式不符合」，而不是 404 —— 这类错最费时间）；
#   2. `#[nik::permission]` 的 `business:hr:employee:sensitive` 真的接了
#      （超管短路会让人以为「好的」，所以必须用一个**没有该码**的账号再打一次）；
#   3. 留痕是**HTTP 层能查到的**：调完接口立刻用 `/api/support/log/audit/page` 反查，
#      断言「谁 / 什么时候 / 谁的什么字段」四个维度都在（审计页看不到的留痕等于没留）；
#   4. 明文接口不会把明文带进**列表 / 详情**（本卡不放宽任何既有出参）。
#
# 用法（隔离实例，端口在隔离段）：
#   BASE_URL=http://127.0.0.1:18097 ADMIN_PASSWORD='Hr13@123456' \
#     bash scripts/verify/hr/employee_sensitive_http_smoke.sh
#
# 环境变量
#   BASE_URL        后端根地址（默认 http://127.0.0.1:18097，隔离段）
#   ADMIN_PASSWORD  超管密码（**没有默认值**：仓库不留默认口令）
#   KEEP=1          结束后不删探针账号（留着排查；员工走软删，本来就留在库里）
# =============================================================================
set -uo pipefail

BASE_URL=${BASE_URL:-http://127.0.0.1:18097}
ADMIN_PASSWORD=${ADMIN_PASSWORD:-}
KEEP=${KEEP:-0}
TAG=$(date +%H%M%S%N | cut -c1-12)
EMP_NO="HR13-SMOKE-$TAG"
ID_CARD="110101199001019876"
BANK="6222021234569876"
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

# 断言：字符串**不**包含
expect_absent() { # name haystack needle
  case "$2" in
    *"$3"*) fail_line "$1：出现了不该出现的 $3" ;;
    *) pass_line "$1（不含 $3）" ;;
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
  local must
  must=$(jq -r '.data.must_change_password // false' "$OUT" 2>/dev/null)
  TOKEN=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
  [ -n "$TOKEN" ] || return 1
  if [ "$must" = "true" ]; then
    req PUT /api/system/auth/password \
      "{\"old_password\":\"$password\",\"new_password\":\"${password}-changed\"}"
    password="${password}-changed"
    # 改密会自增 token_version：带着旧令牌再登录会被 session_guard 挡成 401（"权限已变更"）。
    # 登录接口本身不需要令牌，先清掉再打。
    TOKEN=""
    req POST /api/system/auth/login "{\"username\":\"$user\",\"password\":\"$password\"}"
    TOKEN=$(jq -r '.data.access_token // empty' "$OUT" 2>/dev/null)
    [ -n "$TOKEN" ] || return 1
  fi
  LOGIN_PASSWORD="$password"
  return 0
}

echo "== HR-13 敏感字段明文 + 审计留痕 HTTP 冒烟（$BASE_URL，tag=$TAG）=="

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

# ---- 1. 取部门 + 建夹具员工 ----
req GET /api/system/dept/tree
DEPT_ID=$(jq -r '.data[0].id // empty' "$OUT")
expect_truthy "1. 部门树取到 dept_id" "$DEPT_ID"
if [ -z "$DEPT_ID" ]; then
  echo "FATAL: 库里没有部门，无法新建员工" >&2
  exit 2
fi

req POST /api/business/hr/employee "{
  \"emp_no\":\"$EMP_NO\",\"name\":\"明文冒烟员工\",\"gender\":1,
  \"dept_id\":\"$DEPT_ID\",\"hire_date\":\"2024-03-01\",\"probation_end_date\":\"2024-06-01\",
  \"mobile\":\"13800000013\",\"id_card\":\"$ID_CARD\",
  \"bank_name\":\"工商银行\",\"bank_account\":\"$BANK\",\"bank_account_name\":\"明文冒烟员工\",
  \"remark\":\"HR-13 冒烟夹具\"}"
EMP_ID=$(jq -r '.data.id // empty' "$OUT")
expect_eq "1. 夹具员工已建" 200 "$CODE"
if [ -z "$EMP_ID" ]; then
  echo "FATAL: 夹具员工没建成：$(jq -c '.msg' "$OUT")" >&2
  exit 2
fi
pass_line "1. 夹具员工 id=$EMP_ID emp_no=$EMP_NO"

# ---- 2. 未登录访问明文接口 → 401 ----
saved_token=$TOKEN
TOKEN=""
req GET "/api/business/hr/employee/$EMP_ID/sensitive?field=id_card"
expect_eq "2. 未登录查明文 = 401" 401 "$CODE"
TOKEN=$saved_token

# ---- 3. 列表 / 详情：只有打码值，明文零出现（本卡不放宽既有出参）----
req GET "/api/business/hr/employee/page?page=1&page_size=10&keyword=$EMP_NO"
LIST=$(cat "$OUT")
expect_eq "3. 列表 = 200" 200 "$CODE"
expect_eq "3. 列表命中 1 行" 1 "$(jq -r '.data.total // 0' "$OUT")"
expect_absent "3. 列表不含身份证明文" "$LIST" "$ID_CARD"
expect_absent "3. 列表不含工资卡明文（列表本就不出该列）" "$LIST" "$BANK"

req GET "/api/business/hr/employee/$EMP_ID"
DETAIL=$(cat "$OUT")
expect_eq "3. 详情 = 200" 200 "$CODE"
expect_truthy "3. 详情 sensitive_masked.id_card_masked" "$(jq -r '.data.sensitive_masked.id_card_masked // empty' "$OUT")"
expect_contains "3. 详情身份证是打码形态" "$(jq -r '.data.sensitive_masked.id_card_masked // empty' "$OUT")" "*"
expect_absent "3. 详情不含身份证明文" "$DETAIL" "$ID_CARD"
expect_absent "3. 详情不含工资卡明文" "$DETAIL" "$BANK"

# ---- 4. 明文接口：身份证 ----
req GET "/api/business/hr/employee/$EMP_ID/sensitive?field=id_card"
RESP=$(cat "$OUT")
AUDIT_ID_CARD=$(jq -r '.data.audit_id // empty' "$OUT")
expect_eq "4. #7 查明身份证明文 = 200" 200 "$CODE"
expect_eq "4. 明文 = 库里的原文" "$ID_CARD" "$(jq -r '.data.value // empty' "$OUT")"
expect_eq "4. 回执 field = id_card" "id_card" "$(jq -r '.data.field // empty' "$OUT")"
expect_truthy "4. 回执带 audit_id（本次留痕凭据）" "$AUDIT_ID_CARD"
expect_truthy "4. 回执带 audited_at（数据库时钟）" "$(jq -r '.data.audited_at // empty' "$OUT")"
expect_contains "4. audited_at 是 CST" "$(jq -r '.data.audited_at // empty' "$OUT")" "+08:00"
expect_contains "4. 打码值一并返回（同一规则集合）" "$(jq -r '.data.masked_value // empty' "$OUT")" "*"
expect_eq "4. 回执带工号（人对得上）" "$EMP_NO" "$(jq -r '.data.emp_no // empty' "$OUT")"

# ---- 5. 明文接口：工资卡 ----
req GET "/api/business/hr/employee/$EMP_ID/sensitive?field=bank_account"
AUDIT_ID_BANK=$(jq -r '.data.audit_id // empty' "$OUT")
expect_eq "5. #7 查工资卡明文 = 200" 200 "$CODE"
expect_eq "5. 明文 = 库里的原文" "$BANK" "$(jq -r '.data.value // empty' "$OUT")"
expect_eq "5. 回执 field = bank_account" "bank_account" "$(jq -r '.data.field // empty' "$OUT")"

# ---- 6. 审计留痕：审计页能查到「谁 / 什么时候 / 谁的什么字段」----
sleep 1
req GET "/api/support/log/audit/page?page=1&page_size=20&operation=HR_EMPLOYEE_SENSITIVE_READ"
expect_eq "6. 审计日志页 = 200" 200 "$CODE"
ROWS=$(jq -c --arg eid "$EMP_ID" '[.data.list[] | select(.target_id == $eid)]' "$OUT")
NROWS=$(printf '%s' "$ROWS" | jq 'length')
echo "  ---- 审计行（按 target_id 过滤）----"
printf '%s' "$ROWS" | jq -r '.[] | "    user=\(.username) op=\(.operation) field=\(.detail.field) at=\(.created_at) ip=\(.ip // "-")"'
expect_eq "6. 两条明文访问 = 两条审计" 2 "$NROWS"

# 谁
expect_eq "6. 谁（username 快照）" "admin" "$(printf '%s' "$ROWS" | jq -r '.[0].username // empty')"
expect_truthy "6. 谁（user_id）" "$(printf '%s' "$ROWS" | jq -r '.[0].user_id // empty')"
# 什么时候
expect_truthy "6. 什么时候（created_at）" "$(printf '%s' "$ROWS" | jq -r '.[0].created_at // empty')"
# 看了谁的什么字段
expect_eq "6. 看了谁（target_type）" "Employee" "$(printf '%s' "$ROWS" | jq -r '.[0].target_type // empty')"
expect_eq "6. 看了谁（target_id）" "$EMP_ID" "$(printf '%s' "$ROWS" | jq -r '.[0].target_id // empty')"
# 审计页按 created_at DESC 返回，**不按位置**断言字段：只断言「两个字段各一条」
FIELD_SET=$(printf '%s' "$ROWS" | jq -r '[.[].detail.field] | sort | join(",")')
expect_eq "6. 什么字段（两条分别是 id_card / bank_account）" "bank_account,id_card" "$FIELD_SET"
expect_eq "6. detail 里也带工号" "$EMP_NO" "$(printf '%s' "$ROWS" | jq -r '.[0].detail.emp_no // empty')"
expect_eq "6. detail 里也带姓名" "明文冒烟员工" "$(printf '%s' "$ROWS" | jq -r '.[0].detail.employee_name // empty')"
# 回执的 audit_id 必须能在审计页里找到（「本次查看已记入审计日志」不是空话）
if [ -n "$AUDIT_ID_CARD" ]; then
  FOUND=$(printf '%s' "$ROWS" | jq -r --arg id "$AUDIT_ID_CARD" '[.[] | select(.id == $id)] | length')
  expect_eq "6. 回执 audit_id 能在审计页查到" 1 "$FOUND"
fi
# 审计表不是第二个明文出口
expect_absent "6. 审计行不含身份证明文" "$ROWS" "$ID_CARD"
expect_absent "6. 审计行不含工资卡明文" "$ROWS" "$BANK"

# ---- 7. 非法 field → 400，报文点名合法取值 ----
req GET "/api/business/hr/employee/$EMP_ID/sensitive?field=mobile"
expect_eq "7. field=mobile = 400" 400 "$CODE"
expect_contains "7. 报文列出 id_card" "$(jq -r '.msg // empty' "$OUT")" "id_card"
expect_contains "7. 报文列出 bank_account" "$(jq -r '.msg // empty' "$OUT")" "bank_account"

req GET "/api/business/hr/employee/$EMP_ID/sensitive"
expect_eq "7. 缺 field = 400" 400 "$CODE"

# ---- 8. 无 `:sensitive` 码的账号 → 403（超管短路测不出权限码，必须换账号）----
req POST /api/system/user "{\"username\":\"hr13-probe-$TAG\",\"password\":\"Probe@123456\",\"real_name\":\"HR13 探针\"}"
PROBE_ID=$(jq -r '.data.id // empty' "$OUT")
if [ -n "$PROBE_ID" ]; then
  pass_line "8. 探针账号已建（$PROBE_ID）"
  saved_token=$TOKEN
  if login "hr13-probe-$TAG" "Probe@123456"; then
    req GET "/api/business/hr/employee/$EMP_ID/sensitive?field=id_card"
    expect_eq "8. 无 sensitive 码查明文 = 403" 403 "$CODE"
    req GET "/api/business/hr/employee/$EMP_ID"
    expect_eq "8. 无 detail 码查详情 = 403（对照）" 403 "$CODE"
  else
    fail_line "8. 探针账号登录失败（无法验证权限码）"
  fi
  TOKEN=$saved_token
else
  fail_line "8. 探针账号未建成：$(jq -c '.msg' "$OUT")"
fi

# ---- 9. 软删后查明文 → 404（不留后门）----
req DELETE "/api/business/hr/employee/$EMP_ID"
expect_eq "9. 软删员工 = 200" 200 "$CODE"
req GET "/api/business/hr/employee/$EMP_ID/sensitive?field=id_card"
expect_eq "9. 软删后查明文 = 404" 404 "$CODE"

# ---- 10. 收尾 ----
if [ "$KEEP" != "1" ] && [ -n "${PROBE_ID:-}" ]; then
  req DELETE "/api/system/user/$PROBE_ID"
  [ "$CODE" = "200" ] && pass_line "10. 探针账号已删除" || fail_line "10. 探针账号删除失败（$CODE）"
fi

echo
echo "== 合计：PASS=$pass FAIL=$fail =="
rm -f "$OUT"
[ "$fail" -eq 0 ] || exit 1
exit 0
