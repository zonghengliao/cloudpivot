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
#   A12 完全停掉 Redis（die）→ 受保护 GET 必须 **200**（ADR-20261101 之后不再是 fail-closed 401）
#   A13 无 Redis：读 + 写各打一次 → 全部 200，且写操作在库里查得到
#   A14 无 Redis：登出 200；同一 access 再用 → 401（库侧吊销生效）
#   A15 故障**前**已登出的令牌，Redis 挂掉后再用 → 401（方案 2 会失败、本方案必须过的判据）
#   A16 无 Redis：POST /refresh → 200（P1），旧 refresh 再用 → 401
#   A17 恢复 Redis 后登出：token:blacklist:<jti> 进缓存，且与库一致、行为不变（401）
#   A18 停掉 PostgreSQL（本库拒绝连接）→ 401/500，**绝不 200**（读路径换库后库挂不能变放行）
#   A19 清理任务 SQL：过期 2h 的行被删；未过期与刚过期（<1h）的行保留
#   A20 redis die 期间：/health **仍 200**（liveness），/health/deps **503**（readiness）
#      且 cache.ok=false、auth.revocation_source="db"
#   A21 redis die 期间 /metrics 里 cloudpivot_auth_cache_degraded = 1（未开 metrics 时跳过，不算失败）
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

# 从 JWT 里取一个 claim（A15/A17 要拿 access 的 jti 去查库 / 查缓存键）
jwt_claim() { # jwt_claim <token> <claim>
    [ -n "$1" ] || {
        echo ""
        return 0
    }
    printf '%s' "$1" | cut -d. -f2 | python3 -c '
import base64, json, sys
seg = sys.stdin.read().strip().replace("-", "+").replace("_", "/")
seg += "=" * (-len(seg) % 4)
print(json.loads(base64.b64decode(seg)).get(sys.argv[1], ""))
' "$2"
}

# 登录并写入 LOGIN_TOKEN / LOGIN_REFRESH（多次调用可拿到不同 jti 的令牌，A14~A18 各自独立）
do_login() {
    local pw=${1:-${cur_pw:-$NEW_PASS}}
    api POST /system/auth/login "{\"username\":\"admin\",\"password\":\"$pw\"}" "-"
    LOGIN_TOKEN=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("access_token",""))' 2>/dev/null)
    LOGIN_REFRESH=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("refresh_token",""))' 2>/dev/null)
    LOGIN_CODE=$REQ_CODE
    if [ -z "$LOGIN_TOKEN" ]; then
        bad "重新登录失败（$LOGIN_CODE $REQ_BODY）"
        echo "FATAL: 拿不到令牌，后续断言无意义"
        echo "PASS=$PASS FAIL=$FAIL"
        exit 1
    fi
}

psql_raw() { # psql_raw <db> <sql>
    PGPASSWORD=$PGPASS psql -h "$PGHOST" -U "$PGUSER" -d "$1" -Atc "$2" 2>&1 | head -1
}

# 让「本验证库」不可连接 —— 等价于停掉 PostgreSQL，但只影响本卡的隔离库
# （本机 5432 上还有别的卡的测试库，不能真停服务）。
# ① ALLOW_CONNECTIONS=false 挡住新连接；② 把池里已有的连接全部踢掉。
# 两条都必须连 **postgres 库**执行：
#   - PG 不允许对自己所在的库 `ALLOW_CONNECTIONS false`（实测报
#     `cannot disallow connections for current database`，而且 psql 的报错被
#     重定向到 /dev/null 后看起来像成功 —— 第一版 A18 就是这么假通过的）；
#   - 本库一旦拒绝连接，`psql -d $DB` 也进不去，踢连接会变成空操作。
db_kill() {
    psql_raw postgres "ALTER DATABASE $DB ALLOW_CONNECTIONS false" >/dev/null 2>&1
    psql_raw postgres "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = '$DB' AND pid <> pg_backend_pid()" >/dev/null 2>&1
}

# 恢复连接：本库此刻进不去，所以连 postgres 库改回。
db_restore() {
    psql_raw postgres "ALTER DATABASE $DB ALLOW_CONNECTIONS true" >/dev/null 2>&1
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
# 上一轮若在 A18 中途被打断，本库可能还停在 ALLOW_CONNECTIONS=false：先解锁（库不存在时报错，忽略）
psql_raw postgres "ALTER DATABASE $DB ALLOW_CONNECTIONS true" >/dev/null 2>&1
if ! psql_q "select 1" >/dev/null 2>&1; then
    # 应用账号在本机没有 CREATEDB，退回 postgres 建库（owner 必须是应用账号，
    # 否则启动迁移会只打一行「数据库迁移失败: query failed: : db error」且没有细节）
    PGPASSWORD=$PGPASS createdb -h "$PGHOST" -U "$PGUSER" "$DB" 2>/dev/null ||
        su postgres -c "createdb -O $PGUSER $DB" ||
        echo "（建库失败：可能是库已存在）"
fi
if ! psql_q "select 1" >/dev/null 2>&1; then
    echo "FATAL: 连不上库 $DB（先确认 127.0.0.1:5432 可用、账号 $PGUSER 有权限）"
    exit 1
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
# 复跑友好：上一轮若连续登录失败把 admin 锁了（阈值 10 次），这里先清零
psql_q "update sys_user set login_fail_count = 0, is_locked = false, lock_time = null where username = 'admin'" >/dev/null
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
        # 改密成功后当前口令就是 NEW_PASS：do_login 后续都用它（用旧口令会 20001，
        # 而且每次都记一次登录失败，十次之后 admin 被锁 → 后面全红）
        cur_pw=$NEW_PASS
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

# ---------------------------------------------------------------- A12 无 Redis：读接口正常
say "A12 完全停掉 Redis → 受保护 GET 必须 200（不再是 fail-closed 401）"
redis_ctl die >/dev/null
api GET "/system/menu/tree" - "$TOKEN"
expect "无 Redis 时 GET /system/menu/tree" "$REQ_CODE" "200"

# ---------------------------------------------------------------- A13 读写都正常
say "A13 无 Redis：读 + 写各打一次（写入必须真的落库）"
api GET "/system/role/list" - "$TOKEN"
expect "无 Redis 时读 /system/role/list" "$REQ_CODE" "200"

TAG13="F03B$(date +%s)"
api POST /system/menu "{\"name\":\"$TAG13-按钮\",\"menu_type\":3,\"auth_code\":\"fix03:$TAG13\",\"sort_order\":0,\"status\":1,\"title\":\"$TAG13-按钮\",\"module_code\":\"testc\"}" "$TOKEN"
expect "无 Redis 时写 POST /system/menu" "$REQ_CODE" "200"
MENU13_ID=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; d=json.load(sys.stdin).get("data") or {}; print(d.get("id",""))' 2>/dev/null)
row13=$(psql_q "select count(*) from sys_menu where id = '$MENU13_ID'::uuid and deleted_at is null")
expect "新建菜单已落库" "$row13" "1"

api PUT "/system/menu/$MENU13_ID" "{\"name\":\"$TAG13-改名\",\"menu_type\":3,\"auth_code\":\"fix03:$TAG13\",\"sort_order\":5,\"status\":1,\"title\":\"$TAG13-改名\",\"module_code\":\"testc\"}" "$TOKEN"
expect "无 Redis 时写 PUT /system/menu/{id}" "$REQ_CODE" "200"
name13=$(psql_q "select name from sys_menu where id = '$MENU13_ID'::uuid")
expect "改写已在库里生效" "$name13" "$TAG13-改名"

api DELETE "/system/menu/$MENU13_ID" - "$TOKEN"
expect "无 Redis 时写 DELETE /system/menu/{id}" "$REQ_CODE" "200"
alive13=$(psql_q "select count(*) from sys_menu where id = '$MENU13_ID'::uuid and deleted_at is null")
expect "删除已在库里生效（软删）" "$alive13" "0"

# ---------------------------------------------------------------- A14 无 Redis 登出
say "A14 无 Redis：登出 200，同一 access 再用必须 401（库侧吊销生效）"
do_login
T_A14=$LOGIN_TOKEN
[ -n "$T_A14" ] && ok "无 Redis 时登录成功（access 长度 ${#T_A14}）" || bad "无 Redis 时登录失败（$LOGIN_CODE $REQ_BODY）"
api POST /system/auth/logout - "$T_A14"
expect "无 Redis 时登出" "$REQ_CODE" "200"
api GET /system/menu/tree - "$T_A14"
expect "登出后同一 access 再用" "$REQ_CODE" "401"

# ---------------------------------------------------------------- A15 故障前已登出的令牌
say "A15 故障**前**已登出的令牌：Redis 挂掉后再用必须 401（方案 2 会失败的判据）"
redis_ctl live >/dev/null
do_login
T_A15=$LOGIN_TOKEN
JTI15=$(jwt_claim "$T_A15" jti)
api POST /system/auth/logout - "$T_A15"
expect "故障前登出（Redis 正常）" "$REQ_CODE" "200"
row15=$(psql_q "select count(*) from sys_token_blacklist where jti = '$JTI15'")
expect "库里有该 access jti 的吊销行" "$row15" "1"
redis_ctl die >/dev/null
api GET /system/menu/tree - "$T_A15"
expect "Redis 挂掉后使用故障前已登出的令牌" "$REQ_CODE" "401"

# ---------------------------------------------------------------- A16 无 Redis 刷新
say "A16 无 Redis：POST /system/auth/refresh 必须 200，旧 refresh 再用即 401（P1）"
do_login
R_A16=$LOGIN_REFRESH
api POST /system/auth/refresh "{\"refresh_token\":\"$R_A16\"}" "-"
expect "无 Redis 时 POST /system/auth/refresh" "$REQ_CODE" "200"
NEW_ACCESS=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("access_token",""))' 2>/dev/null)
[ -n "$NEW_ACCESS" ] && ok "无 Redis 时仍签发了新 access（长度 ${#NEW_ACCESS}）" || bad "没拿到新 access（$REQ_BODY）"
api GET /system/menu/tree - "$NEW_ACCESS"
expect "刷新得到的新 access 可用" "$REQ_CODE" "200"
api POST /system/auth/refresh "{\"refresh_token\":\"$R_A16\"}" "-"
expect "旧 refresh 再次使用（库侧已拉黑）" "$REQ_CODE" "401"

# ---------------------------------------------------------------- A17 恢复后缓存与库一致
say "A17 恢复 Redis 后登出：缓存黑名单与库一致，行为不变"
redis_ctl live >/dev/null
do_login
T_A17=$LOGIN_TOKEN
JTI17=$(jwt_claim "$T_A17" jti)
api POST /system/auth/logout - "$T_A17"
expect "恢复后登出" "$REQ_CODE" "200"
dump=$(redis_ctl dump)
expect_contains "缓存里出现 token:blacklist:<jti>" "$dump" "token:blacklist:$JTI17"
row17=$(psql_q "select count(*) from sys_token_blacklist where jti = '$JTI17'")
expect "库里也有该吊销行" "$row17" "1"
api GET /system/menu/tree - "$T_A17"
expect "缓存与库都命中时行为不变（401）" "$REQ_CODE" "401"

# ---------------------------------------------------------------- A18 库挂不能放行
say "A18 本库拒绝连接（等价停 PostgreSQL）→ 必须 401/500，绝不 200"
do_login
T_A18=$LOGIN_TOKEN
db_kill
api GET /system/menu/tree - "$T_A18"
case "$REQ_CODE" in
200) bad "库不可用时仍 200（已吊销判定被旁路）" ;;
401 | 500 | 503) ok "库不可用时 fail-closed（$REQ_CODE）" ;;
*) bad "库不可用时返回 $REQ_CODE（期望 401 / 500）" ;;
esac
db_restore
sleep 2
api GET /system/menu/tree - "$T_A18"
expect "库恢复后同一令牌可用（401 不是永久封禁）" "$REQ_CODE" "200"

# ---------------------------------------------------------------- A19 清理任务
say "A19 清理任务 SQL：过期 2h 的行删掉，未过期与刚过期的保留"
psql_q "INSERT INTO sys_token_blacklist (jti, token_kind, reason, expire_at) VALUES \
('fix03-a19-alive', 'access', 'logout', now() + interval '10 minutes'), \
('fix03-a19-exp60', 'access', 'logout', now() - interval '60 seconds'), \
('fix03-a19-stale2h', 'access', 'logout', now() - interval '2 hours') \
ON CONFLICT (jti) DO NOTHING" >/dev/null
psql_q "DELETE FROM sys_token_blacklist WHERE expire_at < now() - interval '1 hour'" >/dev/null
left19=$(psql_q "select coalesce(string_agg(jti, ',' order by jti), '-') from sys_token_blacklist where jti like 'fix03-a19-%'")
expect "清理后只剩未过期与刚过期" "$left19" "fix03-a19-alive,fix03-a19-exp60"
if grep -q "定时任务 token_blacklist_cleanup" "$LOG"; then
    ok "启动日志里有「定时任务 token_blacklist_cleanup [...] 已排程」"
else
    bad "启动日志里没有 token_blacklist_cleanup 的排程记录（注册点漏了？）"
fi
psql_q "DELETE FROM sys_token_blacklist WHERE jti like 'fix03-a19-%'" >/dev/null

# ---------------------------------------------------------------- A20 健康检查分层
say "A20 redis die 期间：/health 仍 200（liveness），/health/deps 503（readiness）"
redis_ctl die >/dev/null
hc=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")
expect "/health（liveness，永远 200）" "$hc" "200"
# 假 Redis 的 die 只对新命令生效：客户端那条复用连接上前一个已排队的命令仍会被服务完
# （实测：die 之后第一次 /health/deps 仍 ok，第二次才 broken pipe）。所以这里最多探 6 次，
# 以「是否出现过 503」为判据 —— 一次都探不到 503 才是真失败。
dc=000
for _ in $(seq 1 6); do
    dc=$(curl -s -o "$WORK/deps.json" -w '%{http_code}' "http://127.0.0.1:$PORT/health/deps")
    [ "$dc" = "503" ] && break
    sleep 1
done
expect "/health/deps（readiness，依赖不健康即 503）" "$dc" "503"
deps=$(cat "$WORK/deps.json")
expect_contains "cache.ok = false" "$deps" '"cache":{"ok":false'
expect_contains "auth.revocation_source = db" "$deps" '"revocation_source":"db"'
expect_contains "auth.cache_degraded = true" "$deps" '"cache_degraded":true'

# ---------------------------------------------------------------- A21 告警指标
say "A21 redis die 期间 /metrics：cloudpivot_auth_cache_degraded = 1"
mcode=$(curl -s -o "$WORK/metrics.txt" -w '%{http_code}' "http://127.0.0.1:$PORT/metrics")
if [ "$mcode" = "200" ]; then
    mline=$(grep -m1 '^cloudpivot_auth_cache_degraded' "$WORK/metrics.txt" || true)
    if [ -z "$mline" ]; then
        bad "metrics 端点 200 但没有 cloudpivot_auth_cache_degraded（埋点没发）"
    else
        mvalue=$(printf '%s' "$mline" | awk '{print $2}')
        case "$mvalue" in
        1 | 1.0) ok "指标已导出且值为 1：$mline" ;;
        *) bad "cloudpivot_auth_cache_degraded 期望 1，实际「$mline」" ;;
        esac
    fi
    mrows=$(grep -m1 '^cloudpivot_token_blacklist_rows' "$WORK/metrics.txt" || true)
    [ -n "$mrows" ] && ok "指标已导出：$mrows" || bad "缺少 cloudpivot_token_blacklist_rows"
else
    echo "[SKIP] /metrics 未启用（HTTP $mcode）—— A21 不算失败（ADR §7：未开 metrics 时跳过）"
fi
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
