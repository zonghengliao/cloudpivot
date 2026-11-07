#!/usr/bin/env bash
# t_92234f6b / 145 同库二次启动（幂等：created=0 existed=9 dept_backfilled=0）
set -uo pipefail
ENVF=/etc/cloudpivot/cloudpivot-dev.env
DB=${1:-cloudpivot_seed_emp}
PORT=${2:-8053}
TAG=${3:-second}
LOG=/root/backup/seedemp_${TAG}.log

PID=$(ss -ltnp 2>/dev/null | grep ":${PORT} " | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2 || true)
if [ -n "${PID:-}" ]; then kill "$PID"; for i in $(seq 1 30); do ss -ltn | grep -q ":${PORT} " || break; sleep 1; done; fi
echo "port ${PORT} stopped (pid=${PID:-none})"

set -a; . "$ENVF"; set +a
export NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT" NIK_SEED_DEMO_USERS=true
cd /opt/cloudpivot/backend
setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
echo $! > "/root/backup/.seedemp_${TAG}.pid"
for i in $(seq 1 240); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && { echo "health=200 after ${i}s"; break; }
  sleep 1
done
echo "---- 迁移 / 种子日志（真实输出）----"
grep -E "数据库迁移完成|演示账号种子完成|演示部门补漏" "$LOG" | tail -12
