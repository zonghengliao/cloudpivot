#!/usr/bin/env bash
# P1-FIX-11 凭证审批进度验收用隔离实例（本卡私有：库 cloudpivot_fix11a / 端口 18141）
# 用法：bash fix11-instance.sh start|stop
set -uo pipefail
DB=${FIX11_DB:-cloudpivot_fix11a}
PORT=${FIX11_PORT:-18141}
LOG=${FIX11_LOG:-/root/fix11-instance.log}
PIDFILE=/root/fix11-instance.pid
BIN=/opt/cloudpivot/backend/target/debug/cloudpivot

start() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "running pid=$(cat "$PIDFILE") port=$PORT"
    return 0
  fi
  export NIK_PROFILE=test
  export NIK_DATABASE_NAME="$DB"
  export NIK_DATABASE_HOST=127.0.0.1
  export NIK_DATABASE_PORT=5432
  export NIK_DATABASE_USER=alun
  export NIK_DATABASE_PASSWORD=alun
  export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
  export NIK_MIDDLEWARE_AUTH_JWT_SECRET="${FIX11_JWT_SECRET:?need FIX11_JWT_SECRET}"
  export CLOUDPIVOT_STORAGE_MASTER_KEY="${FIX11_STORAGE_KEY:?need FIX11_STORAGE_KEY}"
  export NIK_ADMIN_PASSWORD="${FIX11_ADMIN_PASSWORD:?need FIX11_ADMIN_PASSWORD}"
  export NIK_LOG_LEVEL=info
  rm -f "$LOG"
  cd /opt/cloudpivot/backend || exit 1
  setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDFILE"
  for _ in $(seq 1 300); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
    if [ "$code" = "200" ]; then
      echo "healthy pid=$(cat "$PIDFILE") port=$PORT db=$DB"
      return 0
    fi
    if ! kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
      echo "died"; tail -60 "$LOG"; return 1
    fi
    sleep 1
  done
  echo timeout; tail -60 "$LOG"; return 1
}

stop() {
  if [ -f "$PIDFILE" ]; then
    kill "$(cat "$PIDFILE")" 2>/dev/null
    sleep 2
    kill -9 "$(cat "$PIDFILE")" 2>/dev/null
    rm -f "$PIDFILE"
  fi
  echo stopped
}

case "${1:-start}" in start) start ;; stop) stop ;; *) echo "usage: $0 start|stop"; exit 2 ;; esac
