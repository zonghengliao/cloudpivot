#!/usr/bin/env bash
# 联动规则配置页验收用隔离实例（本卡私有：库 cloudpivot_lnkfe / 端口 18121）
# 用法：bash /root/lnkfe-instance.sh start|stop
set -uo pipefail
DB=${LNKFE_DB:-cloudpivot_lnkfe}
PORT=${LNKFE_PORT:-18121}
LOG=/root/lnkfe-instance.log
PIDFILE=/root/lnkfe-instance.pid
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
  export NIK_DATABASE_USER=e2erole
  export NIK_DATABASE_PASSWORD=e2eiso_2026
  export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
  export NIK_MIDDLEWARE_AUTH_JWT_SECRET="lnkfe-local-secret-0123456789abcdef"
  export CLOUDPIVOT_STORAGE_MASTER_KEY="ea28aca4db6504e829db5548b83fb8c6a4f097ca1d282d23da7b4cc8bf6520a9"
  export NIK_ADMIN_PASSWORD="LinkFe@123456"
  export NIK_LOG_LEVEL=info
  rm -f "$LOG"
  cd /opt/cloudpivot/backend || exit 1
  setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDFILE"
  for _ in $(seq 1 180); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
    if [ "$code" = "200" ]; then
      echo "healthy pid=$(cat "$PIDFILE") port=$PORT db=$DB"
      return 0
    fi
    if ! kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
      echo "died"; tail -40 "$LOG"; return 1
    fi
    sleep 1
  done
  echo timeout; tail -40 "$LOG"; return 1
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
