#!/usr/bin/env bash
# 为 perm_menu_gate.py 建一个「迁移完整」的隔离库并起一次实例（让迁移自动应用）。
# 判据：库里的 sys_menu 必须包含 V0250 种下的 3 条凭证按钮码，否则口径不成立。
set -u
export PATH=/root/.cargo/bin:$PATH
REPO=${REPO:-/opt/cloudpivot}
DB=${DB:-cloudpivot_permgate}
PORT=${PORT:-18190}
BIN=${BIN:-$REPO/backend/target/debug/cloudpivot}
LOG=/tmp/permmenu-gate-server.log
PIDFILE=/tmp/permmenu-gate-server.pid

echo "== 1) 建库 $DB =="
if PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc 'select 1' >/dev/null 2>&1; then
  echo "   已存在"
else
  su postgres -c "createdb -O alun $DB" || { echo "FATAL: createdb 失败"; exit 1; }
  echo "   已创建（owner alun）"
fi

echo "== 2) 起实例跑迁移（$BIN） =="
[ -x "$BIN" ] || { echo "FATAL: 找不到 $BIN，先 cargo build"; exit 1; }
export NIK_PROFILE=test
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_DATABASE_NAME="$DB"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="pmg$(openssl rand -hex 30)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_ADMIN_PASSWORD='PmGate@Admin123'
unset NIK_CACHE_TYPE NIK_REDIS_ENABLED NIK_REDIS_URL

cd "$REPO/backend" || exit 1
: > "$LOG"
setsid "$BIN" > "$LOG" 2>&1 </dev/null &
echo $! > "$PIDFILE"
code=""
for _ in $(seq 1 180); do
  code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)
  [ "$code" = "200" ] && break
  sleep 1
done
echo "   pid=$(cat "$PIDFILE") health=$code"
if [ "$code" != "200" ]; then
  echo "FATAL: 实例没起来，日志尾部："
  tail -25 "$LOG"
  exit 1
fi
grep -E "数据库迁移完成" "$LOG" | tail -1 || echo "   （日志里没有「数据库迁移完成」）"

echo "== 3) 停实例（按 pid 文件，别 pkill） =="
kill -TERM "$(cat "$PIDFILE")" 2>/dev/null
for _ in $(seq 1 20); do
  kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
  sleep 1
done
kill -KILL "$(cat "$PIDFILE")" 2>/dev/null
rm -f "$PIDFILE"

echo "== 4) 库事实 =="
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "select 'menu_rows=' || count(*) from sys_menu where deleted_at is null"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "select 'codes=' || count(distinct auth_code) from sys_menu where deleted_at is null and auth_code is not null and auth_code<>''"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "select 'voucher_buttons=' || count(*) from sys_menu where auth_code in ('business:erp:voucher:audit','business:erp:voucher:post','business:erp:voucher:un-audit')"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "select 'max_migration=' || max(version::text) from schema_migrations"
