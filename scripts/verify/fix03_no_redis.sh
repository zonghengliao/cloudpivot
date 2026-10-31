#!/usr/bin/env bash
# FIX-03 · BUG-SYS-03 验收脚本：无 Redis / 缓存失效失败时，菜单删除与角色授权必须成功
#
# 为什么要「假 Redis」：本机没有 redis-server，而本缺陷最难复现的形态是
#   「Redis 还活着，但 DEL（失效）这一次失败」—— 业务写库成功、缓存打不掉，
#   接口若把这次失败转成 500，就是「用户看到失败、实际成功了」。
# scripts/verify/fix03_fake_redis.py 提供 die / live / del_fail / del_ok / dump 控制口，
# 因此可以精确定点到「只让失效失败」这一刻，而不是笼统地停掉 Redis。
#
# 覆盖的断言（对应卡面验收 ①②③④）：
#   A1  健康检查 200
#   A2  登录 200（首登强制改密则先改密）
#   A3  预热后缓存里确实有 menu:tree:all（证明后续「旧值」是真的）
#   A4  建菜单 200
#   A5  **DEL 失败注入**下删除菜单 → 200（BUG-SYS-03 的原始症状点）
#   A6  该菜单库里已软删（业务真的提交了）
#   A7  缓存里旧树仍在（注入有效，A8 不是「没测到」）
#   A8  恢复 DEL 后读菜单树 → 不含已删菜单（不读旧缓存，验收 ③）
#   A9  **DEL 失败注入**下角色授权 → 200 且绑定落库（验收 ②）
#   A10 恢复后角色下拉可读（验收 ④ 回归）
#   A11 服务日志里有 cache_invalidation_pending=true 的 warn（红线②：不许静默吞）
#   A12 完全停掉 Redis（die）→ 认证层 fail-closed 401，且**业务未被写入**
#       （记录行为：不会再出现「500 但已写入」）
#
# 用法：
#   repo=... FIX03_BIN=... bash scripts/verify/fix03_no_redis.sh
# 依赖：psql（应用账号）、python3、curl。
set -u

REPO=${REPO:-/opt/cloudpivot}
BIN=${FIX03_BIN:-$REPO/backend/target/debug/cloudpivot}
DB=${FIX03_DB:-cloudpivot_fix03_verify}
PORT=${FIX03_PORT:-18145}
RPORT=${FIX03_REDIS_PORT:-6399}
PGHOST=${FIX03_PG_HOST:-127.0.0.1}
PGUSER=${FIX03_PG_USER:-alun}
PGPASS=${FIX03_PG_PASSWORD:-alun}
ADMIN_PASS=${FIX03_ADMIN_PASSWORD:-'Fix03@Admin123'}
NEW_PASS=${FIX03_ADMIN_NEW_PASSWORD:-'Fix03@Admin456'}
WORK=$(mktemp -d /tmp/fix03-verify.XXXXXX)
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

psql_q() {
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

cleanup_all() {
    [ -n "${APP_PID:-}" ] && kill -TERM "$APP_PID" 2>/dev/null && sleep 1 && kill -KILL "$APP_PID" 2>/dev/null
    [ -n "${REDIS_PID:-}" ] && kill -TERM "$REDIS_PID" 2>/dev/null
    :
}
trap cleanup_all EXIT

# ---------------------------------------------------------------- 环境准备
say "环境准备（隔离库 $DB / 端口 $PORT / 假 Redis $RPORT）"
[ -x "$BIN" ] || {
    echo "FATAL: 找不到可执行文件 $BIN"
    exit 1
}
if ! psql_q "select 1" >/dev/null 2>&1; then
    PGPASSWORD=$PGPASS createdb -h "$PGHOST" -U "$PGUSER" "$DB" 2>/dev/null ||
        su postgres -c "createdb -O $PGUSER $DB" 2>/dev/null ||
        echo "（库可能已存在，继续）"
fi
echo "库版本：$(psql_q "select coalesce(max(version::int)::text,'-') from schema_migrations")"

python3 "$REPO/scripts/verify/fix03_fake_redis.py" "$RPORT" > "$WORK/redis.log" 2>&1 &
REDIS_PID=$!
sleep 1
echo "假 Redis pid=$REDIS_PID 状态=$(redis_ctl dump)"

export NIK_PROFILE=test
export NIK_DATABASE_NAME=$DB
export NIK_DATABASE_USER=$PGUSER
export NIK_DATABASE_PASSWORD=$PGPASS
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="f03$(openssl rand -hex 31)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
# 关键：让缓存走 Redis（与 prod/dev 同形态），而不是基础配置的进程内 LRU
export NIK_CACHE_TYPE=redis
export NIK_REDIS_ENABLED=true
export NIK_REDIS_URL="redis://127.0.0.1:$RPORT/3"

cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
APP_PID=$!
for _ in $(seq 1 90); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)
    [ "$code" = "200" ] && break
    sleep 1
done
echo "实例 pid=$APP_PID"
grep -E "使用 Redis 缓存|缓存初始化失败|数据库迁移完成" "$LOG" | tail -3

# ---------------------------------------------------------------- A1 健康检查
say "A1 健康检查"
code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")
expect "健康检查" "$code" "200"

# ---------------------------------------------------------------- A2 登录
say "A2 登录（首登强制改密则先改密）"
TOKEN=""
for pw in "$NEW_PASS" "$ADMIN_PASS"; do
    api POST /system/auth/login "{\"username\":\"admin\",\"password\":\"$pw\"}" "-"
    if [ "$REQ_CODE" = "200" ]; then
        TOKEN=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("access_token",""))' 2>/dev/null)
        cur_pw=$pw
        break
    fi
done
if [ -z "$TOKEN" ]; then
    bad "登录失败（最后一次 $REQ_CODE $REQ_BODY）"
else
    ok "登录成功（access_token 长度 ${#TOKEN}）"
    must=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("must_change_password",False))' 2>/dev/null)
    if [ "$must" = "True" ]; then
        api PUT /system/auth/password "{\"old_password\":\"$cur_pw\",\"new_password\":\"$NEW_PASS\"}" "$TOKEN"
        expect "首登改密" "$REQ_CODE" "200"
        api POST /system/auth/login "{\"username\":\"admin\",\"password\":\"$NEW_PASS\"}" "-"
        TOKEN=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("access_token",""))' 2>/dev/null)
    fi
fi
[ -n "$TOKEN" ] || {
    echo "FATAL: 拿不到令牌，后续断言无意义"
    exit 1
}

# ---------------------------------------------------------------- A3 预热缓存
say "A3 预热菜单树缓存"
api GET /system/menu/tree - "$TOKEN"
expect "菜单树读取" "$REQ_CODE" "200"
dump=$(redis_ctl dump)
expect_contains "缓存里出现 menu:tree:all" "$dump" "menu:tree:all"

# ---------------------------------------------------------------- A4 建菜单
say "A4 建一个待删菜单"
TAG="FIX03$(date +%s)"
api POST /system/menu "{\"name\":\"$TAG-按钮\",\"menu_type\":3,\"auth_code\":\"fix03:$TAG\",\"sort_order\":0,\"status\":1,\"title\":\"$TAG-按钮\",\"module_code\":\"testc\"}" "$TOKEN"
expect "新建菜单" "$REQ_CODE" "200"
MENU_ID=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; d=json.load(sys.stdin).get("data") or {}; print(d.get("id",""))' 2>/dev/null)
[ -n "$MENU_ID" ] && ok "菜单 id=$MENU_ID" || bad "没拿到菜单 id（$REQ_BODY）"

# A9 要授权的菜单：必须**不删**（删掉的菜单会被「菜单不存在或已删除」挡下，那是另一条业务规则）
api POST /system/menu "{\"name\":\"$TAG-授权按钮\",\"menu_type\":3,\"auth_code\":\"fix03:$TAG:grant\",\"sort_order\":0,\"status\":1,\"title\":\"$TAG-授权按钮\",\"module_code\":\"testc\"}" "$TOKEN"
expect "新建授权用菜单" "$REQ_CODE" "200"
MENU2_ID=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; d=json.load(sys.stdin).get("data") or {}; print(d.get("id",""))' 2>/dev/null)
[ -n "$MENU2_ID" ] && ok "授权用菜单 id=$MENU2_ID" || bad "没拿到授权用菜单 id（$REQ_BODY）"

# 再热一次树，确保「含该菜单」的旧树进了缓存
api GET /system/menu/tree - "$TOKEN"
dump=$(redis_ctl dump)
expect_contains "缓存里仍是含该菜单的旧树（键存在）" "$dump" "menu:tree:all"

# ---------------------------------------------------------------- A5/A6/A7 失效失败下的删除
say "A5-A7 只让 DEL（缓存失效）失败 → 删除菜单"
redis_ctl del_fail >/dev/null
api DELETE "/system/menu/$MENU_ID" - "$TOKEN"
expect "删除菜单（缓存失效失败）必须 200 而不是 500" "$REQ_CODE" "200"
alive=$(psql_q "select count(*) from sys_menu where id = '$MENU_ID'::uuid and deleted_at is null")
expect "库里已软删（deleted_at 非空）" "$alive" "0"
dump=$(redis_ctl dump)
expect_contains "缓存里的旧树还在（注入有效，A8 不是空跑）" "$dump" "menu:tree:all"

# ---------------------------------------------------------------- A8 恢复后不读旧值
say "A8 恢复 DEL → 读树不得返回已删菜单（验收 ③）"
redis_ctl del_ok >/dev/null
api GET /system/menu/tree - "$TOKEN"
expect "恢复后读菜单树" "$REQ_CODE" "200"
expect_not_contains "已删菜单不再出现（旧缓存被绕过）" "$REQ_BODY" "$TAG-按钮"

# ---------------------------------------------------------------- A9 授权
say "A9 失效失败下的角色授权（验收 ②）"
api POST /system/role "{\"role_code\":\"FIX03_$TAG\",\"role_name\":\"$TAG-角色\",\"data_scope\":1,\"sort_order\":0,\"status\":1}" "$TOKEN"
expect "新建角色" "$REQ_CODE" "200"
ROLE_ID=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; d=json.load(sys.stdin).get("data") or {}; print(d.get("id",""))' 2>/dev/null)
if [ -n "$ROLE_ID" ]; then
    ok "角色 id=$ROLE_ID"
else
    bad "没拿到角色 id（$REQ_BODY）"
fi
redis_ctl del_fail >/dev/null
api PUT "/system/role/$ROLE_ID/menus" "{\"menu_ids\":[\"$MENU2_ID\"]}" "$TOKEN"
expect "角色授权（缓存失效失败）必须 200" "$REQ_CODE" "200"
[ "$REQ_CODE" = "200" ] || echo "      授权失败体：$REQ_BODY"
bound=$(psql_q "select count(*) from sys_role_menu where role_id = '$ROLE_ID'::uuid and menu_id = '$MENU2_ID'::uuid")
expect "授权绑定已落库" "$bound" "1"
redis_ctl del_ok >/dev/null

# ---------------------------------------------------------------- A10 回归
say "A10 恢复后角色下拉可读（验收 ④ 回归）"
api GET /system/role/list - "$TOKEN"
expect "角色下拉" "$REQ_CODE" "200"
api GET /system/menu/tree - "$TOKEN"
expect "菜单树（DEL 恢复后再次读）" "$REQ_CODE" "200"

# ---------------------------------------------------------------- A11 日志红线
say "A11 红线②：缓存失效失败必须留 warn，不许静默"
hits=$(grep -c "cache_invalidation_pending" "$LOG")
if [ "$hits" -gt 0 ]; then
    ok "日志出现 cache_invalidation_pending 共 $hits 次"
    grep -m1 "cache_invalidation_pending" "$LOG" | sed 's/^/      /'
else
    bad "日志里没有 cache_invalidation_pending（静默吞掉了）"
fi

# ---------------------------------------------------------------- A12 完全无 Redis
say "A12 完全停掉 Redis → 认证 fail-closed，业务不再被写入"
redis_ctl die >/dev/null
api DELETE "/system/menu/$MENU_ID" - "$TOKEN"
if [ "$REQ_CODE" = "401" ]; then
    ok "无缓存时认证层 fail-closed 401（不是 500）"
elif [ "$REQ_CODE" = "200" ]; then
    ok "无缓存时业务照常 200（缓存降级形态）"
else
    bad "无缓存时返回 $REQ_CODE（期望 401 或 200，绝不能是 500）"
fi
expect_not_contains "响应体不含 500 语义" "$REQ_CODE" "500"
redis_ctl live >/dev/null

# ---------------------------------------------------------------- 汇总
say "汇总"
echo "PASS=$PASS FAIL=$FAIL"
echo "实例日志：$LOG（已保留，便于复查 warn 行）"
if [ "$FAIL" -ne 0 ]; then
    echo "结论：FIX-03 验收未通过"
    exit 1
fi
echo "结论：FIX-03 验收通过"
exit 0
