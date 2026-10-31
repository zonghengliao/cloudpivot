#!/usr/bin/env bash
# FIX-06 · 死权限码验收脚本（BUG-SYS-05 的反向扫描 + 权限闸实证）
#
# 卡面验收项 → 本脚本断言：
#   ① 重新扫描 → 死码 0 个（只允许 C 类例外，且例外必须有 TODO 台账）      → A3 / A4 / A8
#   ② 补注解/有注解的接口：无权限角色调用 → 403；管理员 → 200                → A5 / A6
#   ③ 删掉的菜单：侧栏不显示（/system/auth/menus 不含），库里查不到          → A7
#   ④ 不变量套件不退：n10 + N14–N18 全绿                                     → A8
#
# 为什么要独立库：扫描口径依赖「迁移后的真实菜单」。在开发库上跑会被测试残留的
# 菜单干扰（一次性夹具菜单会被判成死码），必须用「迁移后零夹具」的干净库。
#
# 用法：
#   bash scripts/verify/fix06_dead_permissions.sh
# 可覆盖环境变量：REPO / FIX06_BIN / FIX06_DB / FIX06_PORT / FIX06_PG_* / FIX06_SKIP_TESTS=1
set -u

REPO=${REPO:-/opt/cloudpivot}
BIN=${FIX06_BIN:-$REPO/backend/target/debug/cloudpivot}
DB=${FIX06_DB:-cloudpivot_fix06_verify}
PORT=${FIX06_PORT:-18146}
PGHOST=${FIX06_PG_HOST:-127.0.0.1}
PGUSER=${FIX06_PG_USER:-alun}
PGPASS=${FIX06_PG_PASSWORD:-alun}
ADMIN_PASS=${FIX06_ADMIN_PASSWORD:-'Fix06@Admin123'}
ADMIN_NEW_PASS=${FIX06_ADMIN_NEW_PASSWORD:-'Fix06@Admin456'}
USER_PASS=${FIX06_USER_PASSWORD:-'Fix06@User123'}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/fix06-verify.XXXXXX")
LOG=$WORK/server.log
BASE="http://127.0.0.1:$PORT/api"

# C 类例外：后端无校验点，权限点由前端 router meta.permission 拦截（台账见 docs/dead-permissions-20261101.md）
EXCEPTIONS="system:preference:view"

# V0173 下架的 5 条死码（菜单已删）
REMOVED_CODES="business:erp:category:list business:erp:category:detail business:erp:category:create business:erp:purchase-order:detail system:change-log:clean"

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
expect_not_contains() {
    case "$2" in
    *"$3"*) bad "$1（不该出现「$3」）" ;;
    *) ok "$1" ;;
    esac
}

psql_q() { # psql_q <SQL> → 首行
    PGPASSWORD=$PGPASS psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1" 2>&1 | head -1
}

api() { # api <METHOD> <PATH> <BODY|-> [TOKEN]
    local method=$1 path=$2 body=$3 token=${4:-}
    local args=(-s -o "$WORK/body.json" -w '%{http_code}' -X "$method" "$BASE$path")
    if [ -n "$token" ]; then args+=(-H "Authorization: Bearer $token"); fi
    if [ "$body" != "-" ]; then
        args+=(-H 'Content-Type: application/json' -d "$body")
    fi
    REQ_CODE=$(curl "${args[@]}")
    REQ_BODY=$(cat "$WORK/body.json")
}

body_field() { # body_field <a.b.c>
    BODY=$WORK/body.json python3 -c '
import json, os, sys
try:
    data = json.load(open(os.environ["BODY"], encoding="utf-8"))
except Exception:
    print(""); raise SystemExit
for key in sys.argv[1].split("."):
    data = data.get(key) if isinstance(data, dict) else None
    if data is None:
        break
print("" if data is None else ("true" if data is True else ("false" if data is False else data)))
' "$1"
}

body_len() { # 最近一次响应体里 data 是数组时的长度（不是数组则 -1）
    BODY=$WORK/body.json python3 -c '
import json, os
try:
    data = json.load(open(os.environ["BODY"], encoding="utf-8")).get("data")
except Exception:
    print(-1); raise SystemExit
print(len(data) if isinstance(data, list) else -1)
'
}

login() { # login <用户名> <密码> → token
    api POST /system/auth/login "{\"username\":\"$1\",\"password\":\"$2\"}" -
    body_field data.access_token
}

stop_app() {
    if [ -n "${APP_PID:-}" ]; then
        kill -TERM "$APP_PID" 2>/dev/null
        sleep 1
        kill -KILL "$APP_PID" 2>/dev/null
    fi
    APP_PID=""
}
cleanup_all() { stop_app; }
trap cleanup_all EXIT

# ---------------------------------------------------------------- 环境准备
say "环境准备（隔离库 $DB / 端口 $PORT / 无缓存）"
[ -x "$BIN" ] || {
    echo "FATAL: 找不到可执行文件 $BIN（先 cd backend && cargo build）"
    exit 1
}
if [ "$(psql_q 'select 1')" != "1" ]; then
    PGPASSWORD=$PGPASS createdb -h "$PGHOST" -U "$PGUSER" "$DB" 2>/dev/null ||
        su postgres -c "createdb -O $PGUSER $DB" 2>/dev/null ||
        echo "（库可能已存在，继续）"
fi
export NIK_PROFILE=test
export NIK_DATABASE_NAME=$DB
export NIK_DATABASE_USER=$PGUSER
export NIK_DATABASE_PASSWORD=$PGPASS
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="f06$(openssl rand -hex 31)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
unset NIK_CACHE_TYPE NIK_REDIS_ENABLED NIK_REDIS_URL

cd "$REPO/backend" || exit 1
: >"$LOG"
setsid "$BIN" >"$LOG" 2>&1 </dev/null &
APP_PID=$!
code=""
for _ in $(seq 1 120); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)
    [ "$code" = "200" ] && break
    sleep 1
done
echo "实例 pid=$APP_PID 健康码=$code"
if [ "$code" != "200" ]; then
    echo "FATAL: 实例没起来，日志尾巴："
    tail -20 "$LOG"
    exit 1
fi
grep -E "数据库迁移完成" "$LOG" | tail -1 || echo "（日志里没有「数据库迁移完成」，看下一行）"

# ---------------------------------------------------------------- A1 健康
say "A1 健康检查"
expect "健康检查" "$code" "200"

# ---------------------------------------------------------------- A2 管理员登录
say "A2 管理员登录（首登强制改密则先改密）"
TOKEN=""
for pw in "$ADMIN_PASS" "$ADMIN_NEW_PASS"; do
    TOKEN=$(login admin "$pw")
    if [ -n "$TOKEN" ]; then
        ADMIN_PW=$pw
        break
    fi
done
if [ -z "$TOKEN" ]; then
    bad "管理员登录失败（最后一次 $REQ_CODE $REQ_BODY）"
    echo "FATAL: 拿不到管理员令牌"
    exit 1
fi
ok "管理员登录成功（token 长度 ${#TOKEN}）"
if [ "$(body_field data.must_change_password)" = "true" ]; then
    api PUT /system/auth/password "{\"old_password\":\"$ADMIN_PW\",\"new_password\":\"$ADMIN_NEW_PASS\"}" "$TOKEN"
    expect "首登强制改密" "$REQ_CODE" "200"
    ADMIN_PW=$ADMIN_NEW_PASS
    TOKEN=$(login admin "$ADMIN_PW")
    [ -n "$TOKEN" ] && ok "改密后重新登录成功" || bad "改密后重新登录失败"
fi

# ---------------------------------------------------------------- A3 反向扫描
say "A3 反向扫描（菜单 auth_code → 后端源码是否有人问）"
psql_q "select 1" >/dev/null || {
    echo "FATAL: 连不上库"
    exit 1
}
TOTAL=$(psql_q "select count(distinct auth_code) from sys_menu where deleted_at is null and auth_code is not null and auth_code <> ''")
ROWS=$(psql_q "select count(*) from sys_menu where deleted_at is null and auth_code is not null and auth_code <> ''")
echo "菜单权限码：$TOTAL 个去重码 / $ROWS 条菜单行"
[ "${TOTAL:-0}" -ge 150 ] && ok "权限码基数正常（$TOTAL ≥ 150，扫描不是空转）" ||
    bad "权限码基数异常（$TOTAL）"

DEAD=""
while IFS= read -r c; do
    [ -z "$c" ] && continue
    case " $EXCEPTIONS " in *" $c "*) continue ;; esac
    # 剥离注释口径由单测 N14 负责（见 A8）；这里用字面量口径做快速体检
    n=$(grep -rn --fixed-strings "$c" "$REPO/backend/src" --include='*.rs' 2>/dev/null |
        grep -v 'tests\.rs:' | grep -v 'tests_support' | wc -l)
    [ "$n" = "0" ] && DEAD="$DEAD $c"
done < <(psql_q "select distinct auth_code from sys_menu where deleted_at is null and auth_code is not null and auth_code <> '' order by 1" |
    sed 's/\r//')
if [ -z "$DEAD" ]; then
    ok "死码 0 个（例外仅 ${EXCEPTIONS}，已在台账登记）"
else
    bad "扫描到死码：$DEAD"
fi

# ---------------------------------------------------------------- A4 库侧不变量
say "A4 库侧不变量（模块名当权限码 / 已下架的死码 / 授权孤儿绑定）"
BADFORM=$(psql_q "select count(*) from sys_menu where deleted_at is null and auth_code is not null and auth_code <> ''
                  and (length(auth_code) - length(replace(auth_code, ':', ''))) < 2")
expect "权限码形态合规（≥2 个冒号，无「模块名当权限码」）" "$BADFORM" "0"

for c in $REMOVED_CODES; do
    left=$(psql_q "select count(*) from sys_menu where deleted_at is null and auth_code = '$c'")
    expect "已下架的死码 $c 不在菜单里" "$left" "0"
done

ORPHAN=$(psql_q "select count(*) from sys_role_menu rm left join sys_menu m on m.id = rm.menu_id where m.id is null")
expect "sys_role_menu 无孤儿绑定（指向已删菜单）" "$ORPHAN" "0"

# ---------------------------------------------------------------- A5/A6 权限闸
say "A5 造夹具（无权限角色 + 用户）"
TAG=$(date +%s)
ROLE_CODE="FIX06R_$TAG"
USERNAME="fix06_u_$TAG"
ROLE_ID=$(psql_q "insert into sys_role (id, role_code, role_name, status, data_scope)
                  values (gen_random_uuid(), '$ROLE_CODE', 'FIX06 无权限角色', 1, 1) returning id::text")
[ -n "$ROLE_ID" ] && ok "无权限角色 id=$ROLE_ID（一条菜单都没勾）" || {
    bad "造角色失败"
    exit 1
}
api POST /system/user "{\"username\":\"$USERNAME\",\"password\":\"$USER_PASS\",\"real_name\":\"FIX06 无权限用户\",\"role_ids\":[\"$ROLE_ID\"]}" "$TOKEN"
expect "创建无权限用户" "$REQ_CODE" "200"
USER_ID=$(body_field data.id)
[ -n "$USER_ID" ] || {
    bad "没拿到用户 id（$REQ_BODY）"
    exit 1
}
ok "用户 id=$USER_ID"

say "A5 权限闸实证：同一接口，管理员 200 / 无权限用户 403"
api GET /system/menu/tree - "$TOKEN"
expect "管理员 GET /system/menu/tree（system:menu:list）" "$REQ_CODE" "200"
api GET /system/menu/tree - ""
expect "无令牌 GET /system/menu/tree" "$REQ_CODE" "401"

TOKEN_U=$(login "$USERNAME" "$USER_PASS")
if [ -z "$TOKEN_U" ]; then
    bad "无权限用户登录失败（$REQ_CODE $REQ_BODY）"
else
    ok "无权限用户登录成功"
    api GET /system/menu/tree - "$TOKEN_U"
    expect "无权限用户 GET /system/menu/tree → 403（注解闸生效）" "$REQ_CODE" "403"
    expect_contains "403 文案点明缺哪个权限码" "$REQ_BODY" "system:menu:list"

    api GET /system/user/page - "$TOKEN_U"
    expect "无权限用户 GET /system/user/page → 403" "$REQ_CODE" "403"
    # 反例对照：这些码是行级校验（不是注解闸），无权限用户仍可读自己的东西 → 200
    api GET /system/auth/me - "$TOKEN_U"
    expect "对照：自助接口 /system/auth/me → 200（不需要权限码）" "$REQ_CODE" "200"
fi

# ---------------------------------------------------------------- A7 侧栏
say "A7 侧栏：下架的菜单不出现；无权限用户菜单为空"
api GET /system/auth/menus - "$TOKEN"
expect "管理员取侧栏菜单" "$REQ_CODE" "200"
ADMIN_MENUS=$REQ_BODY
for c in $REMOVED_CODES; do
    expect_not_contains "侧栏里没有已下架权限码 $c" "$ADMIN_MENUS" "$c"
done
if [ -n "$TOKEN_U" ]; then
    api GET /system/auth/menus - "$TOKEN_U"
    expect "无权限用户取侧栏菜单" "$REQ_CODE" "200"
    cnt=$(body_len)
    if [ "$cnt" = "0" ]; then
        ok "无权限用户侧栏为空（菜单按授权过滤）"
    else
        bad "无权限用户侧栏不为空（data 条数=$cnt）：$(printf '%s' "$REQ_BODY" | head -c 200)"
    fi
fi

# ---------------------------------------------------------------- A8 不变量套件
say "A8 单测：n10（正向）+ N14–N18（反向）"
if [ "${FIX06_SKIP_TESTS:-0}" = "1" ]; then
    echo "（FIX06_SKIP_TESTS=1，跳过单测）"
else
    TESTOUT=$WORK/tests.log
    if SYS_DB_URL="postgres://$PGUSER:$PGPASS@$PGHOST:5432/$DB" \
        timeout 900 cargo test --lib -- \
        system::menu::tests::n10 \
        system::menu::permission_audit_tests \
        --nocapture --test-threads=1 >"$TESTOUT" 2>&1; then
        ok "n10 + N14–N18 全绿（$(sed -nE 's/^test result: ok\. ([0-9]+) passed.*/\1/p' "$TESTOUT" | head -1) 条）"
        sed -nE 's/.*(\[N1[0-9]\] .*✓).*/\1/p' "$TESTOUT" | sort -u || true
    else
        bad "单测未通过，日志尾巴："
        tail -25 "$TESTOUT"
    fi
fi

# ---------------------------------------------------------------- 收尾
say "汇总"
printf 'PASS=%d FAIL=%d\n' "$PASS" "$FAIL"
printf '（隔离库 %s 保留供排查；删库：su postgres -c "dropdb %s"）\n' "$DB" "$DB"
[ "$FAIL" -eq 0 ] || exit 1
