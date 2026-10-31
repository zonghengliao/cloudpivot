#!/usr/bin/env bash
# FIX-05 · BUG-SYS-04 验收脚本：权限撤销后旧令牌必须立刻 401
#
# 卡面验收项与本脚本断言的对应关系：
#   ① 用户 A 登录拿 token                → A2 / A5
#   ② 撤掉 A 的某个角色                  → A6
#   ③ 旧 token 调受保护接口 → 401「权限已变更，请重新登录」 → A7（含 code=20008）
#   ④ A 重新登录 → 新 token 正常          → A8
#   ⑤ 改角色权限（不是改用户角色）也能触发 → A10
#   ⑥ 超管改自己权限不影响其他用户        → A14（admin 的令牌全程有效）
#   ⑦ 无 Redis 时也生效（降级查库）        → B 段（重启成「无缓存」形态再跑一遍 ②③）
#
# 额外证据（红线 3「不许影响正常请求性能」）：
#   - A3/A9/A12 读假 Redis 的键列表，证明 `uv:{user_id}` 真的被写入缓存；
#   - A11 证明撤销后该键被删（写穿失效，不等 60 s TTL）。
#
# 用法：
#   bash scripts/verify/fix05_token_version.sh
# 可覆盖的环境变量：REPO / FIX05_BIN / FIX05_DB / FIX05_PORT / FIX05_REDIS_PORT / FIX05_PG_*
set -u

REPO=${REPO:-/opt/cloudpivot}
BIN=${FIX05_BIN:-$REPO/backend/target/debug/cloudpivot}
DB=${FIX05_DB:-cloudpivot_fix05_verify}
PORT=${FIX05_PORT:-18146}
RPORT=${FIX05_REDIS_PORT:-6499}
PGHOST=${FIX05_PG_HOST:-127.0.0.1}
PGUSER=${FIX05_PG_USER:-alun}
PGPASS=${FIX05_PG_PASSWORD:-alun}
ADMIN_PASS=${FIX05_ADMIN_PASSWORD:-'Fix05@Admin123'}
ADMIN_NEW_PASS=${FIX05_ADMIN_NEW_PASSWORD:-'Fix05@Admin456'}
USER_PASS=${FIX05_USER_PASSWORD:-'Fix05@User123'}
WORK=$(mktemp -d /tmp/fix05-verify.XXXXXX)
LOG=$WORK/server.log
BASE="http://127.0.0.1:$PORT/api"

PASS=0
FAIL=0

say() { printf '\n=== %s ===\n' "$*"; }
ok() {
    PASS=$((PASS + 1))
    printf '[PASS] %s\n' "$*"
}
bad() {
    FAIL=$((FAIL + 1))
    printf '[FAIL] %s\n' "$*"
}
expect() { # expect <描述> <实际> <期望>
    if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1（期望 $3，实际 $2）"; fi
}
expect_contains() { # expect_contains <描述> <文本> <子串>
    case "$2" in
    *"$3"*) ok "$1" ;;
    *) bad "$1（未命中「$3」；实际：$(printf '%s' "$2" | head -c 300)）" ;;
    esac
}
expect_not_contains() { # expect_not_contains <描述> <文本> <子串>
    case "$2" in
    *"$3"*) bad "$1（不该出现「$3」）" ;;
    *) ok "$1" ;;
    esac
}

psql_q() { # psql_q <SQL> → 首行
    PGPASSWORD=$PGPASS psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1" 2>&1 | head -1
}

redis_ctl() { # redis_ctl die|live|del_fail|del_ok|dump|flush
    python3 - "$RPORT" "$1" <<'PY'
import socket, sys
port = int(sys.argv[1]) + 1
s = socket.create_connection(("127.0.0.1", port), timeout=5)
s.sendall((sys.argv[2] + "\n").encode())
print(s.recv(65536).decode().strip())
s.close()
PY
}

api() { # api <METHOD> <PATH> <BODY|-> [TOKEN]
    local method=$1 path=$2 body=$3 token=${4:-}
    local args=(-s -o "$WORK/body.json" -w '%{http_code}' -X "$method" "$BASE$path")
    [ -n "$token" ] && args+=(-H "Authorization: Bearer $token")
    if [ "$body" != "-" ]; then
        args+=(-H 'Content-Type: application/json' -d "$body")
    fi
    REQ_CODE=$(curl "${args[@]}")
    REQ_BODY=$(cat "$WORK/body.json")
}

body_field() { # body_field <a.b.c>：从最近一次响应体里取值
    BODY=$WORK/body.json python3 - "$1" <<'PY'
import json, os, sys
try:
    with open(os.environ["BODY"], encoding="utf-8") as fh:
        data = json.load(fh)
except Exception:
    print("")
    raise SystemExit
for key in sys.argv[1].split("."):
    if isinstance(data, dict):
        data = data.get(key)
    else:
        data = None
        break
if data is None:
    print("")
elif isinstance(data, bool):
    print("true" if data else "false")
else:
    print(data)
PY
}

login() { # login <用户名> <密码> → 打印 access_token
    api POST /system/auth/login "{\"username\":\"$1\",\"password\":\"$2\"}" "-"
    body_field data.access_token
}

stop_app() {
    [ -n "${APP_PID:-}" ] && kill -TERM "$APP_PID" 2>/dev/null && sleep 1 && kill -KILL "$APP_PID" 2>/dev/null
    APP_PID=""
}

cleanup_all() {
    stop_app
    [ -n "${REDIS_PID:-}" ] && kill -TERM "$REDIS_PID" 2>/dev/null
    :
}
trap cleanup_all EXIT

start_app() { # start_app <with_redis:1|0>
    local with_redis=$1
    export NIK_PROFILE=test
    export NIK_DATABASE_NAME=$DB
    export NIK_DATABASE_USER=$PGUSER
    export NIK_DATABASE_PASSWORD=$PGPASS
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="f05$(openssl rand -hex 31)"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
    export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
    export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
    if [ "$with_redis" = "1" ]; then
        export NIK_CACHE_TYPE=redis
        export NIK_REDIS_ENABLED=true
        export NIK_REDIS_URL="redis://127.0.0.1:$RPORT/5"
    else
        unset NIK_CACHE_TYPE NIK_REDIS_ENABLED NIK_REDIS_URL
    fi
    cd "$REPO/backend" || exit 1
    : >"$LOG"
    setsid nohup "$BIN" >"$LOG" 2>&1 < /dev/null &
    APP_PID=$!
    local code=""
    for _ in $(seq 1 120); do
        code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)
        [ "$code" = "200" ] && break
        sleep 1
    done
    echo "实例 pid=$APP_PID 健康码=$code（缓存形态：$( [ "$with_redis" = 1 ] && echo Redis || echo 无缓存 )）"
    [ "$code" = "200" ] || {
        echo "FATAL: 实例没起来，日志尾巴："
        tail -20 "$LOG"
        exit 1
    }
}

# ---------------------------------------------------------------- 环境准备
say "环境准备（隔离库 $DB / 端口 $PORT / 假 Redis $RPORT）"
[ -x "$BIN" ] || {
    echo "FATAL: 找不到可执行文件 $BIN（先 cargo build）"
    exit 1
}
if [ "$(psql_q "select 1" 2>/dev/null)" != "1" ]; then
    # 应用账号通常没有 CREATEDB：优先自己建，失败再交给 postgres（owner 必须是应用账号，
    # 否则启动迁移会因为表 owner 不是它而只打一行「数据库迁移失败」）
    PGPASSWORD=$PGPASS createdb -h "$PGHOST" -U "$PGUSER" "$DB" 2>/dev/null ||
        su postgres -c "createdb -O $PGUSER $DB" 2>/dev/null ||
        echo "（库可能已存在，继续）"
fi

python3 "$REPO/scripts/verify/fix03_fake_redis.py" "$RPORT" >"$WORK/redis.log" 2>&1 &
REDIS_PID=$!
sleep 1
echo "假 Redis pid=$REDIS_PID 状态=$(redis_ctl dump)"

start_app 1
grep -E "数据库迁移完成|使用 Redis 缓存|缓存初始化失败" "$LOG" | tail -3

# ---------------------------------------------------------------- A1 健康
say "A1 健康检查"
expect "健康检查" "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")" "200"

# ---------------------------------------------------------------- A2 管理员登录
say "A2 管理员登录（首登强制改密则先改密）"
TOKEN=""
for pw in "$ADMIN_NEW_PASS" "$ADMIN_PASS"; do
    TOKEN=$(login admin "$pw")
    [ -n "$TOKEN" ] && {
        ADMIN_PW=$pw
        break
    }
done
if [ -z "$TOKEN" ]; then
    bad "管理员登录失败（最后一次 $REQ_CODE $REQ_BODY）"
    echo "FATAL: 拿不到管理员令牌"
    exit 1
fi
ok "管理员登录成功（access_token 长度 ${#TOKEN}）"
if [ "$(body_field data.must_change_password)" = "true" ]; then
    api PUT /system/auth/password "{\"old_password\":\"$ADMIN_PW\",\"new_password\":\"$ADMIN_NEW_PASS\"}" "$TOKEN"
    expect "首登改密" "$REQ_CODE" "200"
    ADMIN_PW=$ADMIN_NEW_PASS
    TOKEN=$(login admin "$ADMIN_PW")
    [ -n "$TOKEN" ] && ok "改密后重新登录成功" || bad "改密后重新登录失败"
fi
api GET /system/auth/me - "$TOKEN"
expect "管理员 /me" "$REQ_CODE" "200"

# ---------------------------------------------------------------- 夹具：角色 / 用户
say "A3 造夹具（角色 FIX05R + 用户 fix05_u）"
TAG=$(date +%s)
ROLE_CODE="FIX05R_$TAG"
USERNAME="fix05_u_$TAG"
MENU_ID=$(psql_q "select id from sys_menu where auth_code = 'system:user:list' and deleted_at is null limit 1")
[ -n "$MENU_ID" ] || {
    bad "库里找不到 system:user:list 菜单（隔离库迁移没跑完？）"
    exit 1
}
ROLE_ID=$(psql_q "insert into sys_role (id, role_code, role_name, status, data_scope) values (gen_random_uuid(), '$ROLE_CODE', 'FIX05 验收角色', 1, 1) returning id::text" | head -1)
ok "角色 id=$ROLE_ID（role_code=$ROLE_CODE）"
psql_q "insert into sys_role_menu (id, role_id, menu_id) values (gen_random_uuid(), '$ROLE_ID'::uuid, '$MENU_ID'::uuid)" >/dev/null
ok "角色已授予 system:user:list 菜单（改角色权限的起点）"

api POST /system/user "{\"username\":\"$USERNAME\",\"password\":\"$USER_PASS\",\"real_name\":\"FIX05 验收用户\",\"role_ids\":[]}" "$TOKEN"
expect "创建用户 A" "$REQ_CODE" "200"
USER_ID=$(body_field data.id)
[ -n "$USER_ID" ] && ok "用户 id=$USER_ID" || {
    bad "没拿到用户 id（$REQ_BODY）"
    exit 1
}
ver_before=$(psql_q "select token_version from sys_user where id = '$USER_ID'::uuid")
echo "起点 token_version=$ver_before"

# ---------------------------------------------------------------- A4-A5 用户登录
say "A4 用户 A 登录（此时无角色）"
TOKEN_A=$(login "$USERNAME" "$USER_PASS")
[ -n "$TOKEN_A" ] && ok "用户 A 登录成功（长度 ${#TOKEN_A}）" || bad "用户 A 登录失败（$REQ_CODE $REQ_BODY）"
api GET /system/auth/me - "$TOKEN_A"
expect "A5 旧令牌调受保护接口（撤销前应 200）" "$REQ_CODE" "200"
dump=$(redis_ctl dump)
expect_contains "A5 会话字段已进缓存（uv: 键）" "$dump" "uv:"

# ---------------------------------------------------------------- A6 撤掉角色
say "A6 给 A 授予角色 FIX05R（改用户角色 → 必须推进 token_version）"
api PUT "/system/user/$USER_ID/roles" "{\"role_ids\":[\"$ROLE_ID\"]}" "$TOKEN"
expect "分配角色" "$REQ_CODE" "200"
ver_after_grant=$(psql_q "select token_version from sys_user where id = '$USER_ID'::uuid")
[ "$ver_after_grant" -gt "$ver_before" ] &&
    ok "授予角色后 token_version 自增（$ver_before → $ver_after_grant）" ||
    bad "授予角色后 token_version 未自增（$ver_before → $ver_after_grant）"
dump=$(redis_ctl dump)
expect_not_contains "A6 该用户的 uv: 缓存键已被删除（写穿失效）" "$dump" "uv:$USER_ID"

say "A7 撤掉 A 的角色 → 旧令牌必须 401 + 20008"
api PUT "/system/user/$USER_ID/roles" '{"role_ids":[]}' "$TOKEN"
expect "撤销角色" "$REQ_CODE" "200"
api GET /system/auth/me - "$TOKEN_A"
expect "A7 旧令牌调受保护接口" "$REQ_CODE" "401"
expect "A7 业务码必须是 20008" "$(body_field code)" "20008"
expect_contains "A7 文案「您的权限已变更，请重新登录」" "$REQ_BODY" "您的权限已变更，请重新登录"

say "A8 A 重新登录 → 新令牌正常"
TOKEN_A=$(login "$USERNAME" "$USER_PASS")
[ -n "$TOKEN_A" ] && ok "重新登录成功" || bad "重新登录失败（$REQ_CODE $REQ_BODY）"
api GET /system/auth/me - "$TOKEN_A"
expect "A8 新令牌调受保护接口" "$REQ_CODE" "200"

# ---------------------------------------------------------------- A10 改角色权限
say "A10 改角色权限（不是改用户角色）也必须触发"
api PUT "/system/user/$USER_ID/roles" "{\"role_ids\":[\"$ROLE_ID\"]}" "$TOKEN"
expect "（先把角色授回给 A）" "$REQ_CODE" "200"
TOKEN_A=$(login "$USERNAME" "$USER_PASS")
api GET /system/auth/me - "$TOKEN_A"
expect "授权后新令牌可用" "$REQ_CODE" "200"
ver_before_menu=$(psql_q "select token_version from sys_user where id = '$USER_ID'::uuid")
api PUT "/system/role/$ROLE_ID/menus" "{\"menu_ids\":[\"$MENU_ID\"]}" "$TOKEN"
expect "改角色权限（重新授予同一菜单，绑定重建）" "$REQ_CODE" "200"
ver_after_menu=$(psql_q "select token_version from sys_user where id = '$USER_ID'::uuid")
[ "$ver_after_menu" -gt "$ver_before_menu" ] &&
    ok "改角色权限后持有者 token_version 自增（$ver_before_menu → $ver_after_menu）" ||
    bad "改角色权限后持有者 token_version 未自增（$ver_before_menu → $ver_after_menu）"
api GET /system/auth/me - "$TOKEN_A"
expect "A10 改角色权限后旧令牌" "$REQ_CODE" "401"
expect "A10 业务码" "$(body_field code)" "20008"

# ---------------------------------------------------------------- A12 停用
say "A12 停用用户 → 旧令牌必须 401"
TOKEN_A=$(login "$USERNAME" "$USER_PASS")
api GET /system/auth/me - "$TOKEN_A"
expect "停用前令牌可用" "$REQ_CODE" "200"
ver_before_disable=$(psql_q "select token_version from sys_user where id = '$USER_ID'::uuid")
api PATCH "/system/user/$USER_ID/status" '{"status":0}' "$TOKEN"
expect "停用用户" "$REQ_CODE" "200"
ver_after_disable=$(psql_q "select token_version from sys_user where id = '$USER_ID'::uuid")
[ "$ver_after_disable" -gt "$ver_before_disable" ] &&
    ok "停用后 token_version 自增（$ver_before_disable → $ver_after_disable）" ||
    bad "停用后 token_version 未自增（$ver_before_disable → $ver_after_disable）"
api GET /system/auth/me - "$TOKEN_A"
expect "A12 停用后旧令牌" "$REQ_CODE" "401"
expect "A12 业务码" "$(body_field code)" "20008"
api PATCH "/system/user/$USER_ID/status" '{"status":1}' "$TOKEN"
expect "重新启用（后续用例需要）" "$REQ_CODE" "200"

# ---------------------------------------------------------------- A13 改密
say "A13 管理员重置口令 → 旧令牌必须 401"
NEW_USER_PASS="Fix05@Reset$TAG"
TOKEN_A=$(login "$USERNAME" "$USER_PASS")
api GET /system/auth/me - "$TOKEN_A"
expect "改密前令牌可用" "$REQ_CODE" "200"
ver_before_pwd=$(psql_q "select token_version from sys_user where id = '$USER_ID'::uuid")
api PATCH "/system/user/$USER_ID/password" "{\"password\":\"$NEW_USER_PASS\"}" "$TOKEN"
expect "管理员重置口令" "$REQ_CODE" "200"
ver_after_pwd=$(psql_q "select token_version from sys_user where id = '$USER_ID'::uuid")
[ "$ver_after_pwd" -gt "$ver_before_pwd" ] &&
    ok "改密后 token_version 自增（$ver_before_pwd → $ver_after_pwd）" ||
    bad "改密后 token_version 未自增（$ver_before_pwd → $ver_after_pwd）"
api GET /system/auth/me - "$TOKEN_A"
expect "A13 改密后旧令牌" "$REQ_CODE" "401"
expect "A13 业务码" "$(body_field code)" "20008"

# ---------------------------------------------------------------- A14 超管不受影响
say "A14 超管改自己权限不影响其他用户 / 管理员令牌全程有效"
api GET /system/auth/me - "$TOKEN"
expect "管理员令牌在全部变更后仍可用" "$REQ_CODE" "200"
api GET "/system/user/page?page=1&page_size=1" - "$TOKEN"
expect "管理员业务接口仍可用（未被误踢）" "$REQ_CODE" "200"
ver_admin=$(psql_q "select token_version from sys_user where username = 'admin'")
echo "管理员 token_version=$ver_admin（未参与普通用户的批量推进）"

# ---------------------------------------------------------------- B 段：无 Redis
say "B 无 Redis（降级查库）时同样生效"
stop_app
start_app 0
grep -E "使用 Redis 缓存|缓存不可用|缓存初始化失败" "$LOG" | tail -2
TOKEN=$(login admin "$ADMIN_PW")
[ -n "$TOKEN" ] && ok "无缓存形态下管理员登录成功" || bad "无缓存形态下管理员登录失败（$REQ_CODE $REQ_BODY）"
api POST /system/user "{\"username\":\"${USERNAME}_n\",\"password\":\"$USER_PASS\",\"real_name\":\"FIX05 无缓存用户\",\"role_ids\":[\"$ROLE_ID\"]}" "$TOKEN"
expect "创建用户 A'（无缓存形态）" "$REQ_CODE" "200"
USER_ID_N=$(body_field data.id)
TOKEN_N=$(login "${USERNAME}_n" "$USER_PASS")
api GET /system/auth/me - "$TOKEN_N"
expect "B1 无缓存：撤销前令牌可用" "$REQ_CODE" "200"
api PUT "/system/user/$USER_ID_N/roles" '{"role_ids":[]}' "$TOKEN"
expect "B2 无缓存：撤销角色" "$REQ_CODE" "200"
api GET /system/auth/me - "$TOKEN_N"
expect "B3 无缓存：旧令牌仍必须 401（降级查库）" "$REQ_CODE" "401"
expect "B3 业务码" "$(body_field code)" "20008"

# ---------------------------------------------------------------- 收尾
say "收尾：清理夹具"
stop_app
psql_q "delete from sys_user_role where role_id = '$ROLE_ID'::uuid" >/dev/null
psql_q "delete from sys_role_menu where role_id = '$ROLE_ID'::uuid" >/dev/null
psql_q "delete from sys_role where id = '$ROLE_ID'::uuid" >/dev/null
psql_q "delete from sys_user where username like '${USERNAME}%'" >/dev/null
echo "残留检查：$(psql_q "select count(*) from sys_user where username like '${USERNAME}%'") 个夹具用户 / $(psql_q "select count(*) from sys_role where role_code = '$ROLE_CODE'") 个夹具角色"

say "汇总"
printf 'PASS=%d FAIL=%d\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
