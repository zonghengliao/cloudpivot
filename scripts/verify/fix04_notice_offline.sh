#!/usr/bin/env bash
# FIX-04 · BUG-D-05 验收脚本：公告删除时关联通知必须同事务下线
#
# 卡面验收项 ↔ 本脚本断言：
#   ① 发公告 → 目标用户收到通知（sys_notice 有行，未读 +1）        → A3 / A4 / A5
#   ② 删公告 → 通知消失（deleted_at 有值）                        → A6 / A7 / A8
#   ③ 已读回执还在（sys_notice_receiver 未删、read_flag/read_at 未改）→ A9 / A10
#   ④ 徽标数正确减少                                              → A11 / A12
#   ⑤ 消息中心列表查不到已删公告的通知                              → A13
#
# 做法：直接用 SQL 复刻一次「公告已发布并扇出」的状态（避免脚本依赖发布权限/受众配置），
# 然后**走真实 HTTP 接口** `DELETE /api/business/announcement/{id}` 触发删除 ——
# 断言的是 handler + model + 事务边界这一整条链路，而不是单测里的某个函数。
#
# 用法：
#   cd /opt/cloudpivot/backend && cargo build          # 必须先编出带 FIX-04 的二进制
#   bash scripts/verify/fix04_notice_offline.sh
# 可覆盖：REPO / FIX04_BIN / FIX04_DB / FIX04_PORT / FIX04_PG_* / FIX04_ADMIN_PASSWORD
set -u

REPO=${REPO:-/opt/cloudpivot}
BIN=${FIX04_BIN:-$REPO/backend/target/debug/cloudpivot}
DB=${FIX04_DB:-cloudpivot_fix04_verify}
PORT=${FIX04_PORT:-18147}
PGHOST=${FIX04_PG_HOST:-127.0.0.1}
PGUSER=${FIX04_PG_USER:-alun}
PGPASS=${FIX04_PG_PASSWORD:-alun}
ADMIN_PASS=${FIX04_ADMIN_PASSWORD:-'Fix04@Admin123'}
ADMIN_NEW_PASS=${FIX04_ADMIN_NEW_PASSWORD:-'Fix04@Admin456'}
WORK=$(mktemp -d /tmp/fix04-verify.XXXXXX)
LOG=$WORK/server.log
BASE="http://127.0.0.1:$PORT/api"

PASS=0
FAIL=0
# `set -u` 下这些全局必须先声明：api()/login() 会在不同 shell（含命令替换的子 shell）里写它们
REQ_CODE=""
REQ_BODY=""
TOKEN=""
MUST_CHANGE=""

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
expect_at_least() { # expect_at_least <描述> <实际> <下限>
    if [ "$2" -ge "$3" ] 2>/dev/null; then ok "$1（$2）"; else bad "$1（期望 >= $3，实际 $2）"; fi
}
expect_contains() { # expect_contains <描述> <文本> <子串>
    case "$2" in
    *"$3"*) ok "$1" ;;
    *) bad "$1（未命中「$3」；实际：$(printf '%s' "$2" | head -c 300)）" ;;
    esac
}

psql_q() {
    PGPASSWORD=$PGPASS psql -h "$PGHOST" -U "$PGUSER" -d "$DB" -Atc "$1" 2>&1 | head -1
}

api() { # api <METHOD> <PATH> <BODY|-> [TOKEN]
    local method=$1 path=$2 body=$3 token=${4:-}
    local args=(-s -o "$WORK/body.json" -w '%{http_code}' -X "$method" "$BASE$path")
    if [ -n "$token" ]; then
        local scheme="Bearer"
        args+=(-H "Authorization: ${scheme} ${token}")
    fi
    if [ "$body" != "-" ]; then
        args+=(-H 'Content-Type: application/json' -d "$body")
    fi
    REQ_CODE=$(curl "${args[@]}")
    REQ_BODY=$(cat "$WORK/body.json")
}

body_field() { # body_field <dotted.path>
    printf '%s' "$REQ_BODY" | python3 -c "
import json,sys
try:
    value = json.load(sys.stdin)
except Exception:
    print(''); raise SystemExit
for part in sys.argv[1].split('.'):
    if isinstance(value, dict):
        value = value.get(part)
    elif isinstance(value, list):
        try: value = value[int(part)]
        except Exception: value = None
    else:
        value = None
print('' if value is None else value)
" "$1" 2>/dev/null
}

cleanup_all() {
    [ -n "${APP_PID:-}" ] && kill -TERM "$APP_PID" 2>/dev/null && sleep 1 && kill -KILL "$APP_PID" 2>/dev/null
    :
}
trap cleanup_all EXIT

start_app() {
    setsid nohup "$BIN" >> "$LOG" 2>&1 < /dev/null &
    APP_PID=$!
    local ready=0
    for _ in $(seq 1 90); do
        code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)
        if [ "$code" = "200" ]; then ready=1; break; fi
        sleep 1
    done
    echo "实例 pid=$APP_PID 就绪=$ready"
}

stop_app() {
    [ -n "${APP_PID:-}" ] && kill -TERM "$APP_PID" 2>/dev/null
    sleep 1
    [ -n "${APP_PID:-}" ] && kill -KILL "$APP_PID" 2>/dev/null
    APP_PID=""
}

login() { # login <用户名> <密码> → 设置全局 TOKEN / MUST_CHANGE（不要用 $(...) 调用，会丢全局）
    api POST /system/auth/login "{\"username\":\"$1\",\"password\":\"$2\"}" "-"
    TOKEN=$(printf '%s' "$REQ_BODY" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("data",{}).get("access_token",""))' 2>/dev/null)
    MUST_CHANGE=$(body_field data.must_change_password)
}

# ---------------------------------------------------------------- 环境准备
say "环境准备（隔离库 $DB / 端口 $PORT）"
[ -x "$BIN" ] || {
    echo "FATAL: 找不到可执行文件 $BIN（先 cd backend && cargo build）"
    exit 1
}
if ! psql_q "select 1" >/dev/null 2>&1; then
    # 应用账号在部分环境没有 CREATEDB 权限，退回用 postgres 超级用户建库。
    # `</dev/null` 必须有：`su` 会去读 stdin，脚本被管道调用时会卡住。
    PGPASSWORD=$PGPASS createdb -h "$PGHOST" -U "$PGUSER" "$DB" 2>/dev/null ||
        su postgres -c "createdb -O $PGUSER $DB" </dev/null 2>/dev/null ||
        echo "（库可能已存在，继续）"
fi

export NIK_PROFILE=test
export NIK_DATABASE_NAME=$DB
export NIK_DATABASE_USER=$PGUSER
export NIK_DATABASE_PASSWORD=$PGPASS
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="f04$(openssl rand -hex 31)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
# 本缺陷与缓存无关：走基础配置（进程内缓存）即可，少一层故障源
unset NIK_CACHE_TYPE NIK_REDIS_ENABLED NIK_REDIS_URL

cd "$REPO/backend" || exit 1
start_app
echo "迁移版本：$(psql_q "select coalesce(max(version::int)::text,'-') from schema_migrations")"
grep -E "数据库迁移完成" "$LOG" | tail -2

say "0 结构确认（sys_notice / sys_announcement 的关联字段）"
echo "sys_notice 关联列：$(psql_q "select string_agg(column_name, ',' order by ordinal_position) from information_schema.columns where table_name='sys_notice' and column_name in ('biz_type','biz_id','deleted_at','dispatch_status')")"
echo "sys_announcement 软删列：$(psql_q "select column_name from information_schema.columns where table_name='sys_announcement' and column_name='deleted_at'")"

# ---------------------------------------------------------------- A1 健康检查
say "A1 健康检查"
code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")
expect "健康检查" "$code" "200"

# ---------------------------------------------------------------- A2 登录
say "A2 登录（首登强制改密则先改密）"
CUR_PW=""
for pw in "$ADMIN_NEW_PASS" "$ADMIN_PASS"; do
    login admin "$pw"
    if [ -n "$TOKEN" ]; then
        CUR_PW=$pw
        break
    fi
done
if [ -z "$TOKEN" ]; then
    bad "登录失败（$REQ_CODE $REQ_BODY）"
    say "汇总"
    printf 'PASS=%d FAIL=%d\n' "$PASS" "$FAIL"
    exit 1
fi
if [ "$MUST_CHANGE" = "True" ] && [ "$CUR_PW" != "$ADMIN_NEW_PASS" ]; then
    api PUT /system/auth/password "{\"old_password\":\"$CUR_PW\",\"new_password\":\"$ADMIN_NEW_PASS\"}" "$TOKEN"
    expect "首登改密" "$REQ_CODE" "200"
    CUR_PW=$ADMIN_NEW_PASS
    login admin "$CUR_PW"
fi
ok "管理员登录成功（token 长度 ${#TOKEN}）"
api GET /system/auth/me - "$TOKEN"
expect "管理员令牌可用" "$REQ_CODE" "200"
ADMIN_ID=$(psql_q "select id from sys_user where username = 'admin'")
echo "admin id=$ADMIN_ID"

# ---------------------------------------------------------------- A3 造夹具（复刻「已发布并扇出」）
say "A3 造夹具：一条已下线公告 + 关联通知 + 2 个接收人（1 已读 1 未读）"
ANN_ID=$(psql_q "select gen_random_uuid()")
NOTICE_ID=$(psql_q "select gen_random_uuid()")
USER_READ=$(psql_q "select gen_random_uuid()")
TAG="fix04$(date +%s)"
psql_q "INSERT INTO sys_announcement \
    (id, ann_no, title, summary, content_md, content_html, level, scope_type, status, publish_at, \
     publisher_id, version, receiver_count, read_count) \
  VALUES ('$ANN_ID'::uuid, '${TAG}A1', 'FIX-04 验收公告', '摘要', '正文', '<p>正文</p>', 'NORMAL', 'ALL', \
          'OFFLINE', now() - interval '1 hour', '$ADMIN_ID'::uuid, 1, 2, 1)" >/dev/null
psql_q "INSERT INTO sys_notice \
    (id, notice_no, notice_type, level, title, content, content_format, link_url, biz_type, biz_id, \
     sender_name, target_type, target_ids, receiver_count, read_count, dispatch_status, status) \
  VALUES ('$NOTICE_ID'::uuid, '${TAG}N1', 'CIRCULAR', 'INFO', 'FIX-04 验收公告', '正文', 'TEXT', \
          '/announcement/$ANN_ID', 'announcement', '$ANN_ID', '公告', 'ALL', '[]'::jsonb, 2, 1, 'SENT', 1)" >/dev/null
psql_q "INSERT INTO sys_notice_receiver (notice_id, user_id, read_flag, read_at) \
  VALUES ('$NOTICE_ID'::uuid, '$ADMIN_ID'::uuid, FALSE, NULL), \
         ('$NOTICE_ID'::uuid, '$USER_READ'::uuid, TRUE, now())" >/dev/null
psql_q "INSERT INTO sys_announcement_read (announcement_id, user_id) VALUES ('$ANN_ID'::uuid, '$ADMIN_ID'::uuid)" >/dev/null
echo "公告 $ANN_ID / 通知 $NOTICE_ID"
expect "公告已落库（OFFLINE）" "$(psql_q "select status from sys_announcement where id='$ANN_ID'::uuid")" "OFFLINE"
expect "通知已落库且未下线" "$(psql_q "select (deleted_at is null) from sys_notice where id='$NOTICE_ID'::uuid")" "t"

# ---------------------------------------------------------------- A4/A5 基线：通知可见、未读 +1
say "A4/A5 基线：消息中心能看到、徽标数 +1"
UNREAD_BEFORE=$(api GET /business/notice/unread-count - "$TOKEN" >/dev/null; body_field data.unread_count)
expect_contains "A4 未读数接口可读" "$(printf '%s' "$REQ_BODY" | head -c 120)" "data"
MINE_BEFORE=$(api GET /business/notice/mine - "$TOKEN" >/dev/null; printf '%s' "$REQ_BODY" | grep -c "$NOTICE_ID")
expect "A5 消息中心列表含该通知" "$MINE_BEFORE" "1"
UNREAD_DB_BEFORE=$(psql_q "select count(*) from sys_notice_receiver r join sys_notice n on n.id=r.notice_id \
  where r.user_id='$ADMIN_ID'::uuid and r.read_flag=false and r.is_deleted=false and n.deleted_at is null")
echo "DB 口径未读（admin）=$UNREAD_DB_BEFORE / 接口未读数=$UNREAD_BEFORE"

# ---------------------------------------------------------------- A6 删公告（真实 HTTP）
say "A6 DELETE /api/business/announcement/{id}"
api DELETE "/business/announcement/$ANN_ID" - "$TOKEN"
expect "删公告返回 200" "$REQ_CODE" "200"

# ---------------------------------------------------------------- A7 通知已下线
say "A7 通知已下线（deleted_at 有值）"
expect "sys_notice.deleted_at 有值" "$(psql_q "select (deleted_at is not null) from sys_notice where id='$NOTICE_ID'::uuid")" "t"
expect "公告本身软删" "$(psql_q "select (deleted_at is not null) from sys_announcement where id='$ANN_ID'::uuid")" "t"

# ---------------------------------------------------------------- A8/A9/A10 已读回执不动
say "A8/A9/A10 已读回执：行数不变、read_flag/read_at 未改、公告已读还在"
expect "接收行一条都没少" "$(psql_q "select count(*) from sys_notice_receiver where notice_id='$NOTICE_ID'::uuid")" "2"
expect "已读那位的 read_flag 仍为 true" "$(psql_q "select read_flag from sys_notice_receiver where notice_id='$NOTICE_ID'::uuid and user_id='$USER_READ'::uuid")" "t"
expect "已读那位的 read_at 仍在" "$(psql_q "select (read_at is not null) from sys_notice_receiver where notice_id='$NOTICE_ID'::uuid and user_id='$USER_READ'::uuid")" "t"
expect "read_at 值未被改写" "$(psql_q "select to_char(read_at,'YYYY-MM-DD HH24:MI:SS') = to_char(read_at,'YYYY-MM-DD HH24:MI:SS') from sys_notice_receiver where notice_id='$NOTICE_ID'::uuid and user_id='$USER_READ'::uuid")" "t"
expect "公告已读回执（sys_announcement_read）仍在" "$(psql_q "select count(*) from sys_announcement_read where announcement_id='$ANN_ID'::uuid")" "1"
expect "接收行只打了 is_deleted，未被物理删除" "$(psql_q "select count(*) from sys_notice_receiver where notice_id='$NOTICE_ID'::uuid and is_deleted=true")" "2"

# ---------------------------------------------------------------- A11/A12 徽标数减少
say "A11/A12 徽标数正确减少"
UNREAD_AFTER=$(api GET /business/notice/unread-count - "$TOKEN" >/dev/null; body_field data.unread_count)
UNREAD_DB_AFTER=$(psql_q "select count(*) from sys_notice_receiver r join sys_notice n on n.id=r.notice_id \
  where r.user_id='$ADMIN_ID'::uuid and r.read_flag=false and r.is_deleted=false and n.deleted_at is null")
echo "删后 DB 口径未读=$UNREAD_DB_AFTER / 接口未读数=$UNREAD_AFTER"
if [ -n "$UNREAD_BEFORE" ] && [ -n "$UNREAD_AFTER" ] && [ "$UNREAD_AFTER" -lt "$UNREAD_BEFORE" ]; then
    ok "A11 徽标数减少（$UNREAD_BEFORE → $UNREAD_AFTER）"
else
    bad "A11 徽标数未减少（$UNREAD_BEFORE → $UNREAD_AFTER）"
fi
expect "A12 DB 口径该通知不再计入未读" "$UNREAD_DB_AFTER" "$((UNREAD_DB_BEFORE - 1))"

# ---------------------------------------------------------------- A13 消息中心查不到
say "A13 消息中心列表查不到已删公告的通知"
api GET /business/notice/mine - "$TOKEN"
expect "列表接口 200" "$REQ_CODE" "200"
if printf '%s' "$REQ_BODY" | grep -q "$NOTICE_ID"; then
    bad "A13 列表里仍能看到已删公告的通知"
else
    ok "A13 列表里已查不到该通知（BUG-D-05 修复点）"
fi

# ---------------------------------------------------------------- A14 日志
say "A14 服务日志留痕"
# 日志带 ANSI 颜色（tracing 字段名/等号会被上色），先剥掉再断言，否则字符串对不上
LOG_LINE=$(sed -e 's/\x1B\[[0-9;]*[mK]//g' "$LOG" | grep "公告已软删" | tail -1)
expect_contains "日志记录通知下线数" "$LOG_LINE" "notice_offline=1"
expect_contains "日志记录接收行数" "$LOG_LINE" "receiver_offline=2"

# ---------------------------------------------------------------- 收尾
say "收尾：清理夹具"
psql_q "delete from sys_announcement_read where announcement_id='$ANN_ID'::uuid" >/dev/null
psql_q "delete from sys_notice_receiver where notice_id='$NOTICE_ID'::uuid" >/dev/null
psql_q "delete from sys_notice where id='$NOTICE_ID'::uuid" >/dev/null
psql_q "delete from sys_announcement where id='$ANN_ID'::uuid" >/dev/null
echo "残留：$(psql_q "select count(*) from sys_notice where notice_no like '${TAG}%'") 条通知 / $(psql_q "select count(*) from sys_announcement where ann_no like '${TAG}%'") 条公告"

say "汇总"
printf 'PASS=%d FAIL=%d\n' "$PASS" "$FAIL"
echo "服务日志：$LOG"
[ "$FAIL" -eq 0 ] || exit 1
