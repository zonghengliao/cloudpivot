#!/usr/bin/env bash
# =============================================================================
# HR-17 HTTP 冒烟：`seed-from-users`（dry-run 只报告 / 批量建 / user_id 回写）
# =============================================================================
#
# 为什么还要有它（真库用例已经证明了三条验收项）
# ---------------------------------------------
# `cargo test` 走的是 `seed_from_users()` 这个**业务函数**，不经过 axum 路由、权限宏、
# 请求体反序列化。这几件事只有起真实例才验得到：
#
#   1. `POST /api/business/hr/employee/seed-from-users` 真的注册上了（不会被
#      `/employee/{id}` 那条路由抢走 / 被当成未知路径 404）；
#   2. 未登录是 401（不是「谁都能批量建人事主档」）；
#   3. **请求体只给 `{}`（不传 `dry_run`）时返回预览、不写库** —— 这是本卡的安全缺省，
#      纯代码级单测证明的是 `is_dry_run()` 的返回值，只有真请求能证明「空 body → 不写」；
#   4. 出参字段名与前端要消费的形状对得上（`created` / `skipped` / `failed` / `preview[]`）；
#   5. `dept_id` 传错（不存在 / 非 uuid）是 400 而不是 500。
#
# 用法（隔离实例 + 隔离库）：
#   BASE_URL=http://127.0.0.1:18099 ADMIN_PASSWORD='Admin@123456' \
#     PGPASSWORD=alun PSQL_URL=postgres://alun:alun@127.0.0.1:5432/hr17_card \
#     bash scripts/verify/hr/seed_http_smoke.sh
#
# 环境变量
#   BASE_URL        后端根地址（默认 http://127.0.0.1:18099）
#   ADMIN_PASSWORD  超管密码（**没有默认值**：仓库不留默认口令）
#   PSQL_URL        实例所连的库（造 / 清夹具账号与部门；`sys_user` 的写接口归 system 模块，本卡不改它）
#   KEEP=1          结束后不清理夹具（留着排查）
#
# 注意（空库首启）：`admin` 的 `must_change_password=true`，脚本会先改密再重登，
# 新口令是 `${ADMIN_PASSWORD}-changed` —— **第二次**跑就要传新口令。
# =============================================================================
set -uo pipefail

BASE_URL=${BASE_URL:-http://127.0.0.1:18099}
ADMIN_PASSWORD=${ADMIN_PASSWORD:-}
PSQL_URL=${PSQL_URL:-}
KEEP=${KEEP:-0}
TAG=$(date +%H%M%S%N | cut -c1-12)
OUT=$(mktemp)
CODE=0
TOKEN=""
pass=0
fail=0

DEPT_CODE="HR17E${TAG}"
USER_OK="hr17e.ok.${TAG}"
USER_ROBOT="hr17e.robot.${TAG}"
USER_NONAME="hr17e.noname.${TAG}"

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

jqv() { jq -r "$1" "$OUT" 2>/dev/null; }

psql_q() { psql "$PSQL_URL" -qtAX -c "$1" 2>&1; }

req() { # METHOD PATH [BODY]
  local method=$1 path=$2 body=${3:-}
  local args=(-s -o "$OUT" -w '%{http_code}' -X "$method" "$BASE_URL$path" -H 'Content-Type: application/json')
  if [ -n "${TOKEN:-}" ]; then args+=(-H "Authorization: Bearer ${TOKEN}"); fi
  if [ -n "$body" ]; then args+=(-d "$body"); fi
  CODE=$(curl "${args[@]}")
}

login() {
  local user=$1 password=$2
  req POST /api/system/auth/login "{\"username\":\"$user\",\"password\":\"$password\"}"
  local must
  must=$(jqv '.data.must_change_password // false')
  TOKEN=$(jqv '.data.access_token // empty')
  [ -n "$TOKEN" ] || return 1
  if [ "$must" = "true" ]; then
    req PUT /api/system/auth/password \
      "{\"old_password\":\"$password\",\"new_password\":\"${password}-changed\"}"
    TOKEN=""
    req POST /api/system/auth/login "{\"username\":\"$user\",\"password\":\"${password}-changed\"}"
    TOKEN=$(jqv '.data.access_token // empty')
    [ -n "$TOKEN" ] || return 1
  fi
  return 0
}

cleanup() {
  [ "$KEEP" = "1" ] && { echo "KEEP=1：夹具保留（dept=$DEPT_CODE）"; return; }
  psql_q "DELETE FROM biz_employee WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'hr17e.%${TAG}')" >/dev/null
  psql_q "DELETE FROM sys_user WHERE username IN ('$USER_OK','$USER_ROBOT','$USER_NONAME')" >/dev/null
  psql_q "DELETE FROM sys_dept WHERE dept_code = '$DEPT_CODE'" >/dev/null
  echo "夹具已清理（dept=$DEPT_CODE）"
}
trap cleanup EXIT

echo "== HR-17 seed-from-users HTTP 冒烟（$BASE_URL，tag=$TAG）=="

# ---- 0. 前置 ----
if [ -z "$ADMIN_PASSWORD" ]; then
  echo "FATAL: 必须显式给 ADMIN_PASSWORD（仓库不留默认口令）" >&2
  exit 2
fi
if [ -z "$PSQL_URL" ]; then
  echo "FATAL: 必须给 PSQL_URL（造 / 清夹具账号用；sys_user 的写接口属 system 模块）" >&2
  exit 2
fi
CODE=$(curl -s -o /dev/null -w '%{http_code}' -m 5 "$BASE_URL/health")
expect_eq "0. /health 就绪" 200 "$CODE"

TOKEN=""
req POST /api/business/hr/employee/seed-from-users '{}'
expect_eq "0. 未登录批量建档 → 401" 401 "$CODE"

# ---- 1. 造夹具账号（一个正常 / 一个机器人 / 一个没姓名）----
psql_q "INSERT INTO sys_dept (id, dept_code, dept_name, status, sort_order) \
        VALUES (gen_random_uuid(), '$DEPT_CODE', 'HR17 冒烟部门', 1, 991)" >/dev/null
DEPT_ID=$(psql_q "SELECT id FROM sys_dept WHERE dept_code = '$DEPT_CODE' AND deleted_at IS NULL")
if [ -z "$DEPT_ID" ]; then
  echo "FATAL: 造部门失败" >&2
  exit 2
fi
psql_q "INSERT INTO sys_user (id, username, password, real_name, mobile, dept_id, status, user_type) VALUES
        (gen_random_uuid(), '$USER_OK',     'hr17-smoke-not-a-hash', '冒烟正常员工', '13700000001', '$DEPT_ID', 1, 'HUMAN'),
        (gen_random_uuid(), '$USER_ROBOT',  'hr17-smoke-not-a-hash', '冒烟机器人',   '13700000002', '$DEPT_ID', 1, 'ROBOT'),
        (gen_random_uuid(), '$USER_NONAME', 'hr17-smoke-not-a-hash', NULL,           '13700000003', '$DEPT_ID', 1, 'HUMAN')" >/dev/null
CNT=$(psql_q "SELECT count(*) FROM sys_user WHERE username IN ('$USER_OK','$USER_ROBOT','$USER_NONAME')")
expect_eq "1. 夹具账号就位" 3 "$CNT"

if ! login admin "$ADMIN_PASSWORD"; then
  echo "FATAL: admin 登录失败（口令不对？空库首启已改过密？）" >&2
  exit 2
fi

# ---- 2. 空 body（不传 dry_run）→ 只预览，绝不写库 ----
req POST /api/business/hr/employee/seed-from-users '{}'
expect_eq "2. 空 body → 200" 200 "$CODE"
expect_eq "2. dry_run 缺省 = true" true "$(jqv '.data.dry_run')"
expect_eq "2. created 必须是 0（只报告不写库）" 0 "$(jqv '.data.created')"
PLANNED=$(jqv '.data.planned_create')
if [ "${PLANNED:-0}" -ge 1 ]; then pass_line "2. 计划建档数 ≥1（$PLANNED）"; else fail_line "2. 计划建档数 = ${PLANNED:-空}"; fi
# 只看本套三个夹具账号（库里还有迁移种下的其它账号）：正常那个可建、另两个各自被跳过。
expect_eq "2. 三个夹具账号里可建档的条数" 1 \
  "$(jqv '[.data.preview[] | select(.username | startswith("hr17e.")) | select(.action == "CREATE")] | length')"
expect_eq "2. 三个夹具账号里被跳过的条数" 2 \
  "$(jqv '[.data.preview[] | select(.username | startswith("hr17e.")) | select(.action == "SKIP")] | length')"
expect_eq "2. 机器人账号的原因码" "NOT_HUMAN" \
  "$(jqv ".data.preview[] | select(.username == \"$USER_ROBOT\") | .reason")"
expect_eq "2. 没姓名账号的原因码" "NAME_MISSING" \
  "$(jqv ".data.preview[] | select(.username == \"$USER_NONAME\") | .reason")"
expect_eq "2. 预览行的 emp_no 为空（预览不占号）" "null" \
  "$(jqv ".data.preview[] | select(.username == \"$USER_OK\") | .emp_no")"
expect_eq "2. 库里没有该账号的档案" 0 \
  "$(psql_q "SELECT count(*) FROM biz_employee WHERE user_id = (SELECT id FROM sys_user WHERE username = '$USER_OK')")"

# ---- 3. 显式 dry_run=true + dept_id + hire_date → 仍不写库 ----
req POST /api/business/hr/employee/seed-from-users \
  "{\"dept_id\":\"$DEPT_ID\",\"hire_date\":\"2024-02-29\",\"dry_run\":true}"
expect_eq "3. dry_run=true → 200" 200 "$CODE"
expect_eq "3. created=0" 0 "$(jqv '.data.created')"
expect_eq "3. hire_date 回显" "2024-02-29" "$(jqv '.data.hire_date')"
expect_eq "3. 库里仍无档案" 0 \
  "$(psql_q "SELECT count(*) FROM biz_employee WHERE user_id = (SELECT id FROM sys_user WHERE username = '$USER_OK')")"
expect_contains "3. 部门名回显非空" "$(jqv ".data.preview[] | select(.username == \"$USER_OK\") | .dept_name")" "HR17 冒烟部门"

# ---- 4. dry_run=false 落库：user_id 回写 + status=ACTIVE + 跳过不误建 ----
req POST /api/business/hr/employee/seed-from-users \
  "{\"dept_id\":\"$DEPT_ID\",\"hire_date\":\"2024-02-29\",\"dry_run\":false}"
expect_eq "4. dry_run=false → 200" 200 "$CODE"
expect_eq "4. created=1（只有正常账号）" 1 "$(jqv '.data.created')"
expect_eq "4. failed=0" 0 "$(jqv '.data.failed')"
EMP_NO=$(jqv ".data.preview[] | select(.username == \"$USER_OK\") | .emp_no")
expect_contains "4. 回执给了工号" "$EMP_NO" "EMP-"
expect_eq "4. user_id 回写正确" 1 \
  "$(psql_q "SELECT count(*) FROM biz_employee e JOIN sys_user u ON u.id = e.user_id \
             WHERE u.username = '$USER_OK' AND e.deleted_at IS NULL")"
expect_eq "4. 状态是 ACTIVE" "ACTIVE" \
  "$(psql_q "SELECT e.status FROM biz_employee e JOIN sys_user u ON u.id = e.user_id WHERE u.username = '$USER_OK'")"
expect_eq "4. 入职日期取入参" "2024-02-29" \
  "$(psql_q "SELECT e.hire_date FROM biz_employee e JOIN sys_user u ON u.id = e.user_id WHERE u.username = '$USER_OK'")"
expect_eq "4. 部门取账号部门" "$DEPT_ID" \
  "$(psql_q "SELECT e.dept_id FROM biz_employee e JOIN sys_user u ON u.id = e.user_id WHERE u.username = '$USER_OK'")"
expect_eq "4. 机器人 / 没姓名的仍无档案" 0 \
  "$(psql_q "SELECT count(*) FROM biz_employee e JOIN sys_user u ON u.id = e.user_id \
             WHERE u.username IN ('$USER_ROBOT','$USER_NONAME')")"

# ---- 5. 幂等：再点一次，0 新建、全员 HAS_EMPLOYEE ----
req POST /api/business/hr/employee/seed-from-users \
  "{\"dept_id\":\"$DEPT_ID\",\"hire_date\":\"2024-02-29\",\"dry_run\":false}"
expect_eq "5. 重放 → 200" 200 "$CODE"
expect_eq "5. created=0（幂等）" 0 "$(jqv '.data.created')"
expect_eq "5. 已建档账号 → HAS_EMPLOYEE" "HAS_EMPLOYEE" \
  "$(jqv ".data.preview[] | select(.username == \"$USER_OK\") | .reason")"
expect_contains "5. 报文点名已有工号" "$(jqv ".data.preview[] | select(.username == \"$USER_OK\") | .message")" "EMP-"

# ---- 6. 入参闸：dept_id 不存在 / 非法 uuid → 400 ----
req POST /api/business/hr/employee/seed-from-users \
  '{"dept_id":"00000000-0000-0000-0000-000000000001","dry_run":true}'
expect_eq "6. dept_id 不存在 → 400" 400 "$CODE"
expect_contains "6. 报文说明部门要真实存在" "$(jqv '.msg')" "不存在或已删除"
req POST /api/business/hr/employee/seed-from-users '{"dept_id":"not-a-uuid","dry_run":true}'
expect_eq "6. dept_id 非 uuid → 400" 400 "$CODE"
req POST /api/business/hr/employee/seed-from-users '{"hire_date":"2024/02/29","dry_run":true}'
expect_eq "6. hire_date 格式错 → 400" 400 "$CODE"
expect_contains "6. 报文给出正确格式" "$(jqv '.msg')" "yyyy-MM-dd"

echo
echo "== 结果：通过 $pass 项 / 失败 $fail 项 =="
[ "$fail" -eq 0 ] || exit 1
