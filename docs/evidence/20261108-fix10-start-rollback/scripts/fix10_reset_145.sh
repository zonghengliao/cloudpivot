#!/usr/bin/env bash
# t_0127714a：重建全新演示库并起隔离实例（任意宿主机，用 DB/PORT/BIN 覆盖；只停目标端口，不碰服役实例）
set -uo pipefail
ENVF=/etc/cloudpivot/cloudpivot-dev.env
DB=${DB:-cloudpivot_fix10_demo}
PORT=${PORT:-8054}
BIN=${BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}
LOG=${LOG:-/root/fix10_expense_${PORT}.log}
say() { printf '%s\n' "$*"; }

PW=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' "$ENVF")
[ -n "$PW" ] || { echo "!! 读不到 $ENVF 里的 NIK_DATABASE_PASSWORD"; exit 1; }
export PGPASSWORD="$PW"

# ① 停目标端口（只按端口找 PID，绝不 pkill 全体）
PID=$(ss -ltnp 2>/dev/null | grep ":${PORT} " | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2 || true)
if [ -n "${PID:-}" ]; then
  kill "$PID" 2>/dev/null
  for _ in $(seq 1 30); do ss -ltn | grep -q ":${PORT} " || break; sleep 1; done
fi
say "==> port ${PORT} stopped (pid=${PID:-none})"

# ② 重建库
su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"DROP DATABASE IF EXISTS $DB\"" >/dev/null || exit 1
su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\"" >/dev/null || exit 1
say "==> db recreated: $DB (owner=cloudpivot)"

# ③ 带演示种子首启（migrations 编译期内嵌在二进制里）
set -a; . "$ENVF"; set +a
export NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT" NIK_SEED_DEMO_USERS=true
cd /opt/cloudpivot/backend || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "/root/fix10_expense_${PORT}.pid"
say "==> started pid=$(cat "/root/fix10_expense_${PORT}.pid") log=$LOG"

for i in $(seq 1 240); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && { say "==> health=200 after ${i}s"; break; }
  sleep 1
done
grep -E "数据库迁移完成|演示账号种子" "$LOG" | tail -3

# ④ 基线
q() { psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "$1"; }
say "migrations=$(q 'select count(*) from schema_migrations')"
say "users=$(q 'select count(*) from sys_user where deleted_at is null')"
say "flows_published=$(q 'select count(*) from biz_flow_def where status='"'"'PUBLISHED'"'"' and deleted_at is null')"
say "expense_types=$(q 'select count(*) from biz_expense_type where deleted_at is null')"
say "expense_reports=$(q 'select count(*) from biz_expense_report')"
say "flow_instances=$(q 'select count(*) from biz_flow_instance')"
