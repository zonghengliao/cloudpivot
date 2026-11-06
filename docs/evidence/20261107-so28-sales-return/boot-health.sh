#!/usr/bin/env bash
# ============================================================================
# SO-28 启动自洽：在**已迁移**的库上再起一次实例
#   期望：日志「数据库迁移完成: 应用 0 个迁移」（幂等，V0303 不重放）
#        + /health 200 + schema_migrations 条数不增（无重复行）
# 用法：bash docs/evidence/20261107-so28-sales-return/boot-health.sh
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

REPO=${REPO:-/opt/cloudpivot}
DB=${ERP_DB:-cloudpivot_so28}
PORT=${PORT:-18228}
BIN=${BIN:-$REPO/backend/target/debug/cloudpivot}
LOG=${LOG:-/root/so28-boot.log}
PIDFILE=${PIDFILE:-/root/so28-boot.pid}
export PGPASSWORD=${PGPASSWORD:-alun}
psql_q() { psql -h 127.0.0.1 -U alun -d "$1" -Atq -c "$2"; }

BEFORE=$(psql_q "$DB" "select count(*)||'|'||max(version::int) from schema_migrations")
echo "== 启动前 schema_migrations(count|max) = $BEFORE"

export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"

: > "$LOG"
cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"
echo "== 启动 pid=$(cat "$PIDFILE") 端口=$PORT（库=$DB）"

ok=0
for _ in $(seq 1 120); do
  grep -q "数据库迁移完成" "$LOG" 2>/dev/null && { ok=1; break; }
  kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
  sleep 1
done
[ "$ok" = "1" ] || { echo "FAIL: 未等到「数据库迁移完成」，日志尾部："; tail -20 "$LOG"; kill "$(cat "$PIDFILE")" 2>/dev/null; exit 1; }

grep "数据库迁移完成" "$LOG" | tail -1 | sed 's/^/   /'

HEALTH=""
for _ in $(seq 1 60); do
  HEALTH=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)
  [ "$HEALTH" = "200" ] && break
  sleep 1
done
echo "== /health = $HEALTH"
AFTER=$(psql_q "$DB" "select count(*)||'|'||max(version::int) from schema_migrations")
echo "== 启动后 schema_migrations(count|max) = $AFTER"

kill "$(cat "$PIDFILE")" 2>/dev/null
sleep 2

rc=0
if [ "$HEALTH" != "200" ]; then echo "FAIL: /health = $HEALTH"; rc=1; fi
if [ "$BEFORE" != "$AFTER" ]; then echo "FAIL: 迁移条数变化 $BEFORE → $AFTER（重复应用？）"; rc=1; fi
if ! grep -q "应用 0 个迁移" "$LOG"; then echo "FAIL: 启动日志未出现「应用 0 个迁移」"; rc=1; fi
[ "$rc" = "0" ] && echo "ALL PASS: 应用 0 个迁移 + /health 200 + 迁移条数不变"
exit $rc
