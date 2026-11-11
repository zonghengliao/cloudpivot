#!/usr/bin/env bash
# =============================================================================
# HR-14 HTTP 冒烟：工号取号（`GET /business/hr/employee/next-no` + 新建自动取号）
# =============================================================================
#
# 为什么还要有它（真库用例已经证明取号不重不跳了）
# -----------------------------------------------
# `cargo test` 走 handler 里的**业务函数**，不经过 axum 路由与权限宏。三件事只有起真实例才验得到：
#
#   1. `GET /business/hr/employee/next-no` 真的注册上了，且**不会被** `/employee/{id}` 抢走
#      （宏路径拼错 → 落 SPA fallback 返回 HTML；被 `{id}` 抢走 → `parse_uuid("next-no")` 报 400）；
#   2. 未登录是 401（路由/权限中间件在位），不是「预览无人管」；
#   3. 预览 → 新建 → 再预览，**跨请求**的号是连续 +1 的（前端按钮的两次点击就这个链路），
#      而手工工号**不**消耗号段。
#
# 用法（隔离实例）：
#   BASE_URL=http://127.0.0.1:18099 ADMIN_PASSWORD='Admin@123456' \
#     bash scripts/verify/hr/employee_no_http_smoke.sh
#
# 环境变量
#   BASE_URL        后端根地址（默认 http://127.0.0.1:18099，隔离段）
#   ADMIN_PASSWORD  超管密码（**没有默认值**：仓库不留默认口令）
#   KEEP=1          结束后不软删冒烟员工（留着排查；默认走 DELETE 软删，行仍在库里带 deleted_at）
#
# 注意（空库首启）：`admin` 的 `must_change_password=true`，脚本会先改密再重登，
# 新口令是 `${ADMIN_PASSWORD}-changed` —— **第二次**跑就要传新口令，否则登录失败退出 2。
# =============================================================================
set -uo pipefail

BASE_URL=${BASE_URL:-http://127.0.0.1:18099}
ADMIN_PASSWORD=${ADMIN_PASSWORD:-}
KEEP=${KEEP:-0}
TAG=$(date +%H%M%S%N | cut -c1-12)
MANUAL_NO="HR14-SMOKE-$TAG"
ID_CARD="110101$TAG"
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

# 断言：字符串匹配
expect_contains() { # name haystack needle
  case "$2" in
    *"$3"*) pass_line "$1（含 $3）" ;;
    *) fail_line "$1：期望包含 $3，实际 ${2:0:200}" ;;
  esac
}

# 断言：字符串**不**匹配
expect_absent() { # name haystack needle
  case "$2" in
    *"$3"*) fail_line "$1：出现了不该出现的 $3" ;;
    *) pass_line "$1（不含 $3）" ;;
  esac
}

# 通用请求：req METHOD PATH [BODY]，结果落 $OUT / $CODE
req() {
  local method=$1 path=$2 body=${3:-}
  local args=(-s -o "$OUT" -w '%{http_code}' -X "$method" "$BASE_URL$path" -H 'Content-Type: application/json')
  [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
  [ -n "$body" ] && args+=(-d "$body")
  CODE=$(curl "${args[@]}")
}

# 登录（必改密的账号先改密再重登；改密会自增 token_version，旧令牌必须先清掉）
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

# 取工号流水（EMP-0007 → 7）
seq_of() { printf '%s' "$1" | sed -n 's/^EMP-0*\([0-9]\+\)$/\1/p'; }

echo "== HR-14 工号取号 HTTP 冒烟（$BASE_URL，tag=$TAG）=="

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

# ---- 1. 未登录预览 → 401 ----
saved_token=$TOKEN
TOKEN=""
req GET /api/business/hr/employee/next-no
expect_eq "1. 未登录预览工号 = 401" 401 "$CODE"
TOKEN=$saved_token

# ---- 2. 预览：形态正确、连打两次同值（不耗号）----
req GET /api/business/hr/employee/next-no
expect_eq "2. #9 预览工号 = 200（路由已注册，没被 /{id} 抢走）" 200 "$CODE"
PREVIEW1=$(jq -r '.data.next_no // empty' "$OUT")
expect_contains "2. 形态是 EMP-xxxx" "$PREVIEW1" "EMP-"
N1=$(seq_of "$PREVIEW1")
[ -n "$N1" ] && pass_line "2. 能解析出流水（$N1）" || fail_line "2. 工号流水解析失败：$PREVIEW1"

req GET /api/business/hr/employee/next-no
PREVIEW2=$(jq -r '.data.next_no // empty' "$OUT")
expect_eq "2. 连打两次是同一个号（预览不耗号）" "$PREVIEW1" "$PREVIEW2"

# ---- 3. 新建（**不传 emp_no**）→ 自动取号 ----
req GET /api/system/dept/tree
DEPT_ID=$(jq -r '.data[0].id // empty' "$OUT")
if [ -z "$DEPT_ID" ]; then
  echo "FATAL: 库里没有部门，无法新建员工" >&2
  exit 2
fi
pass_line "3. 部门树取到 dept_id（$DEPT_ID）"

req POST /api/business/hr/employee "{
  \"name\":\"取号冒烟员工$TAG\",\"gender\":1,\"dept_id\":\"$DEPT_ID\",
  \"hire_date\":\"2024-03-01\",\"mobile\":\"13800000014\",\"id_card\":\"$ID_CARD\",
  \"remark\":\"HR-14 冒烟夹具 $TAG\"}"
AUTO1=$(jq -r '.data.emp_no // empty' "$OUT")
AUTO1_ID=$(jq -r '.data.id // empty' "$OUT")
expect_eq "3. 不传 emp_no 也能建（自动取号）= 200" 200 "$CODE"
expect_eq "3. 回执工号 = 上一步预览值（同一实例、无人抢号）" "$PREVIEW1" "$AUTO1"
if [ -z "$AUTO1_ID" ]; then
  echo "FATAL: 自动取号员工没建成：$(jq -c '.msg' "$OUT")" >&2
  exit 2
fi
pass_line "3. 自动取号员工 id=$AUTO1_ID emp_no=$AUTO1"

# ---- 4. 再预览 = 上一个 +1（不跳号）----
req GET /api/business/hr/employee/next-no
PREVIEW3=$(jq -r '.data.next_no // empty' "$OUT")
N2=$(seq_of "$AUTO1")
N3=$(seq_of "$PREVIEW3")
expect_eq "4. 下一个号 = 已用工号 +1（不跳号）" "$((N2 + 1))" "$N3"
expect_eq "4. 下一个号 ≠ 已用工号（不重复）" 1 "$([ "$PREVIEW3" != "$AUTO1" ] && echo 1 || echo 0)"

# ---- 5. 手工工号：原样保留，不消耗号段 ----
req POST /api/business/hr/employee "{
  \"emp_no\":\"$MANUAL_NO\",\"name\":\"手工工号员工$TAG\",\"gender\":2,\"dept_id\":\"$DEPT_ID\",
  \"hire_date\":\"2024-03-01\",\"mobile\":\"13800000015\",\"id_card\":\"110102$TAG\",
  \"remark\":\"HR-14 冒烟夹具（手工号）$TAG\"}"
MANUAL_ID=$(jq -r '.data.id // empty' "$OUT")
expect_eq "5. 手工工号新建 = 200" 200 "$CODE"
expect_eq "5. 手工工号原样保留" "$MANUAL_NO" "$(jq -r '.data.emp_no // empty' "$OUT")"

req GET /api/business/hr/employee/next-no
expect_eq "5. 手工工号不消耗号段（再预览还是 $PREVIEW3）" "$PREVIEW3" "$(jq -r '.data.next_no // empty' "$OUT")"

# ---- 6. 重复工号 → 409 + 68001（唯一键仍然生效，取号器不会绕开它）----
req POST /api/business/hr/employee "{
  \"emp_no\":\"$MANUAL_NO\",\"name\":\"重复工号员工$TAG\",\"gender\":1,\"dept_id\":\"$DEPT_ID\",
  \"hire_date\":\"2024-03-01\",\"mobile\":\"13800000016\",\"id_card\":\"110103$TAG\",
  \"remark\":\"HR-14 冒烟夹具（重复号）$TAG\"}"
expect_eq "6. 重复工号 = 409" 409 "$CODE"
expect_eq "6. 错误码 = 68001（工号冲突）" 68001 "$(jq -r '.code // 0' "$OUT")"
expect_contains "6. 报文点名工号" "$(jq -r '.msg // empty' "$OUT")" "$MANUAL_NO"

# ---- 7. 不传工的无效载荷：400（不是 500）----
req POST /api/business/hr/employee "{\"name\":\"没部门$TAG\",\"hire_date\":\"2024-03-01\"}"
expect_eq "7. 缺 dept_id = 400（点名列名，不是 500）" 400 "$CODE"
expect_contains "7. 报文点 dept_id" "$(jq -r '.msg // empty' "$OUT")" "dept_id"
expect_absent "7. 报文不泄内部细节" "$(cat "$OUT")" "panicked"

# ---- 8. 收尾：软删冒烟员工（KEEP=1 保留）----
if [ "$KEEP" = "1" ]; then
  echo "  [SKIP] KEEP=1：保留冒烟员工（$AUTO1_ID / $MANUAL_ID）"
else
  req DELETE "/api/business/hr/employee/$AUTO1_ID"
  expect_eq "8. 软删自动取号员工 = 200" 200 "$CODE"
  req DELETE "/api/business/hr/employee/$MANUAL_ID"
  expect_eq "8. 软删手工工号员工 = 200" 200 "$CODE"
fi

echo "== 结果：PASS=$pass FAIL=$fail =="
rm -f "$OUT"
[ "$fail" -eq 0 ] || exit 1
