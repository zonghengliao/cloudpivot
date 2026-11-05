#!/usr/bin/env bash
# P1-FIX-11（t_066e9f2d）凭证审批进度验收用隔离实例
#   DB=cloudpivot_fix11b（克隆自 cloudpivot_test，启动时补齐迁移到最新）PORT=18156
# 用法：bash fix11-db-instance.sh start|stop|status
set -uo pipefail
DB=${FIX11_DB:-cloudpivot_fix11b}
PORT=${FIX11_PORT:-18156}
LOG=${FIX11_LOG:-/root/fix11b-instance.log}
PIDFILE=/root/fix11b-instance.pid
BIN=${FIX11_BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}

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
  # profile=test 安全守卫：JWT secret / 存储主密钥缺一个就 FATAL（迁移也跑不了）
  export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
  export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
  export NIK_ADMIN_PASSWORD="${FIX11_ADMIN_PASSWORD:?need FIX11_ADMIN_PASSWORD}"   # 仓库里不落口令
  export NIK_LOG_LEVEL=info
  : > "$LOG"
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
      echo "died"; tail -80 "$LOG"; return 1
    fi
    sleep 1
  done
  echo timeout; tail -80 "$LOG"; return 1
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

case "${1:-start}" in
  start) start ;;
  stop) stop ;;
  status) curl -s -o /dev/null -w "health=%{http_code}\n" "http://127.0.0.1:$PORT/health" ;;
  *) echo "usage: $0 start|stop|status"; exit 2 ;;
esac
