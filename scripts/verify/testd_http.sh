#!/usr/bin/env bash
# 测试D · HTTP 层越权验证（真服务 + 真库 + 真中间件）
#
# 为什么需要它：`cargo test --test testd` 里 I09 / MO-01 只能验到「令牌不带权限码 ⇒
# has_permission=false」这一层（集成测试进程没有 axum 路由栈）。真正的 403 是
# `#[nik::permission]` 中间件在**服务进程**里拦下来的，只能在起服务之后用 HTTP 证。
#
# 用法（先起服务，profile=test → cloudpivot_test）：
#   cd backend
#   NIK_PROFILE=test NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=*** \
#   NIK_MIDDLEWARE_AUTH_JWT_SECRET=... CLOUDPIVOT_STORAGE_MASTER_KEY=... \
#   cargo run -- profile=test
#
#   # 另开一个 shell：
#   PGPASSWORD=*** bash scripts/verify/testd_http.sh
#
# 环境变量：
#   BASE         接口根（默认 http://127.0.0.1:8023/api）
#   ADMIN_USER   超管账号（默认 admin）
#   ADMIN_PASS   超管口令（默认 Admin@123456）
#   TESTD_PSQL   清理用连接串（默认 postgres://alun@127.0.0.1:5432/cloudpivot_test）
#
# 退出码：0 = 全部符合预期（越权全部 403）；1 = 有偏差（会把响应体打出来）。
set -uo pipefail

BASE="${BASE:-http://127.0.0.1:8023/api}"
ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_PASS="${ADMIN_PASS:-Admin@123456}"
TESTD_PSQL="${TESTD_PSQL:-postgres://alun@127.0.0.1:5432/cloudpivot_test}"
PREFIX="${PREFIX:-TDHTTP}"
SUFFIX="$(date +%H%M%S)"
LOW_USER="${PREFIX}-low-${SUFFIX}"
LOW_ROLE="${PREFIX}-LOW-${SUFFIX}"
LOW_PASS="Td@123456"

fail=0
pass=0

# 红线：本脚本会改 `sys_user.must_change_password`（见「准备」），只允许指向 cloudpivot_test。
case "$TESTD_PSQL" in
    *cloudpivot_test*) ;;
    *) printf 'FATAL: TESTD_PSQL 必须指向 cloudpivot_test（当前：%s）\n' "$TESTD_PSQL"; exit 1 ;;
esac

say() { printf '%s\n' "$*"; }

# 取 JSON 字段（不依赖 jq）：json_field <field> <<< "$json"
json_field() {
    local field="$1"
    sed -n "s/.*\"${field}\":\"\([^\"]*\)\".*/\1/p" | head -1
}

http_code() { # http_code <method> <path> <token> [body]
    local method="$1" path="$2" token="$3" body="${4:-}"
    if [ -n "$body" ]; then
        curl -s -o /tmp/testd_http_body.$$ -w '%{http_code}' -X "$method" "$BASE$path" \
            -H "Authorization: Bearer $token" -H 'Content-Type: application/json' -d "$body"
    else
        curl -s -o /tmp/testd_http_body.$$ -w '%{http_code}' -X "$method" "$BASE$path" \
            -H "Authorization: Bearer $token"
    fi
}

expect() { # expect <code> <desc> <actual>
    if [ "$1" = "$3" ]; then
        pass=$((pass + 1))
        say "  ok    [$2] $3"
    else
        fail=$((fail + 1))
        say "  FAIL  [$2] 期望 $1，实际 $3"
        say "        body: $(head -c 300 /tmp/testd_http_body.$$ 2>/dev/null)"
    fi
}

cleanup() {
    # 还原 admin 的「首次登录必须改密」标记（见准备步骤）
    psql "$TESTD_PSQL" -q -v ON_ERROR_STOP=0 >/dev/null 2>&1 <<SQL
UPDATE sys_user SET must_change_password = true WHERE username = '${ADMIN_USER}';
SQL
    psql "$TESTD_PSQL" -q -v ON_ERROR_STOP=0 >/dev/null 2>&1 <<SQL
DELETE FROM sys_change_log WHERE operator_id IN (SELECT id FROM sys_user WHERE username LIKE '${PREFIX}-low-%');
DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE '${PREFIX}-low-%');
DELETE FROM sys_user WHERE username LIKE '${PREFIX}-low-%';
DELETE FROM sys_role WHERE role_code LIKE '${PREFIX}-LOW-%';
SQL
}
trap cleanup EXIT

say "== 测试D HTTP 越权验证（$BASE，前缀 $PREFIX）=="

say "[0] 准备：清掉 admin 的「首次登录必须改密」标记（仅 cloudpivot_test；退出时还原）"
psql "$TESTD_PSQL" -q -v ON_ERROR_STOP=1 -c \
    "UPDATE sys_user SET must_change_password = false WHERE username = '$ADMIN_USER'" >/dev/null

say "[1] 健康检查"
code=$(curl -s -o /tmp/testd_http_body.$$ -w '%{http_code}' "http://127.0.0.1:8023/health")
expect 200 "health" "$code"

TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}" | json_field access_token)
if [ -z "$TOKEN" ]; then
    say "FATAL: 超管登录失败（检查账号口令 / 服务是否 profile=test）"
    exit 1
fi
say "[1] 超管登录成功（token 长度 ${#TOKEN}）"

say "[2] 超管正例（证明功能本身可用，403 不是「接口坏了」）"
expect 200 "admin GET /business/impexp/export/sync" "$(http_code GET '/business/impexp/export/sync?biz_type=system.user' "$TOKEN")"
expect 200 "admin GET /business/monitor/server-status" "$(http_code GET '/business/monitor/server-status' "$TOKEN")"

say "[3] 建低权角色（不绑任何菜单/按钮 → 权限集为空）"
role_code=$(http_code POST '/system/role' "$TOKEN" \
    "{\"role_code\":\"$LOW_ROLE\",\"role_name\":\"测试D 低权角色\",\"status\":1,\"data_scope\":1}")
expect 200 "POST /system/role" "$role_code"
ROLE_ID=$(psql "$TESTD_PSQL" -t -A -c "SELECT id FROM sys_role WHERE role_code='$LOW_ROLE'" 2>/dev/null | head -1)
if [ -z "$ROLE_ID" ]; then
    say "FATAL: 角色没落库（$LOW_ROLE）"
    exit 1
fi
say "  role_id=$ROLE_ID"

say "[4] 建低权用户（只绑该角色）"
expect 200 "POST /system/user" "$(http_code POST '/system/user' "$TOKEN" \
    "{\"username\":\"$LOW_USER\",\"password\":\"$LOW_PASS\",\"real_name\":\"测试D 低权\",\"status\":1,\"role_ids\":[\"$ROLE_ID\"]}")"

LOW_TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$LOW_USER\",\"password\":\"$LOW_PASS\"}" | json_field access_token)
if [ -z "$LOW_TOKEN" ]; then
    say "FATAL: 低权用户登录失败（$LOW_USER）"
    exit 1
fi
say "[5] 低权用户登录成功（token 长度 ${#LOW_TOKEN}）"

say "[6] 越权负例：低权用户打 5 个受保护接口，必须全部 403"
expect 403 "GET  /business/impexp/export/sync（I09 导出权限）" "$(http_code GET '/business/impexp/export/sync?biz_type=system.user' "$LOW_TOKEN")"
expect 403 "GET  /business/impexp/import/template（导入模板）" "$(http_code GET '/business/impexp/import/template?biz_type=system.dict_item' "$LOW_TOKEN")"
expect 403 "GET  /business/impexp/export/page（导出任务列表）" "$(http_code GET '/business/impexp/export/page?page=1&page_size=10' "$LOW_TOKEN")"
expect 403 "GET  /business/monitor/server-status（MO-01 监控）" "$(http_code GET '/business/monitor/server-status' "$LOW_TOKEN")"
expect 403 "POST /business/announcement（G09 谁能发公告）" "$(http_code POST '/business/announcement' "$LOW_TOKEN" \
    '{"title":"TDHTTP 越权尝试","content_md":"x","content_type":"PLAIN","level":"NORMAL","scope_type":"ALL"}')"

say "[7] 低权用户自己的普通接口应正常（证明不是全站 403）"
expect 200 "GET  /system/auth/me" "$(http_code GET '/system/auth/me' "$LOW_TOKEN")"

say ""
say "结果：$pass 通过 / $fail 失败"
[ "$fail" -eq 0 ] || exit 1
