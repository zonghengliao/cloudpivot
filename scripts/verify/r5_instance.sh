#!/usr/bin/env bash
# R5 隔离实例（cloudpivot_r5e @ 127.0.0.1:18098）—— e2e / 页面验收用。
# 起法：bash scripts/verify/r5_instance.sh [start|stop]
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=cloudpivot_r5e
PORT=18098
LOG=/tmp/r5e_server.log
PIDFILE=/tmp/r5e_server.pid
ACTION="${1:-start}"

if [ "$ACTION" = "stop" ]; then
  if [ -f "$PIDFILE" ]; then
    kill "$(cat "$PIDFILE")" 2>/dev/null || true
    sleep 3
    kill -9 "$(cat "$PIDFILE")" 2>/dev/null || true
    echo "stopped $(cat "$PIDFILE")"
  else
    echo "no pid file"
  fi
  exit 0
fi

cd /opt/cloudpivot/backend

if ! runuser -u postgres -- psql -tAc "SELECT 1 FROM pg_database WHERE datname='$DB'" | grep -q 1; then
  runuser -u postgres -- psql -qc "CREATE DATABASE $DB OWNER alun"
  echo "created $DB"
fi

rm -f "$PIDFILE"
NIK_PROFILE=test \
NIK_DATABASE_NAME=$DB \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:$PORT \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"

for i in $(seq 1 150); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:$PORT/health || true)
  if [ "$code" = "200" ]; then break; fi
  if ! kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then echo "实例提前退出"; tail -30 "$LOG"; exit 1; fi
  sleep 2
done

echo "health=$code pid=$(cat "$PIDFILE") db=$DB port=$PORT"
grep -E "数据库迁移完成|V0189" "$LOG" | tail -3
