#!/bin/bash
# E2E-FIX-6 隔离实例（99 本机）：库 cp_e2efix6（空库 → 首启自动应用全部迁移 + 种子），
# 端口 127.0.0.1:18420。二进制来自**当前共享工作区**（本卡改动就在里面）。
#
# 用法：bash 00_start_instance.sh start|status|stop|logs
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

BIN=${BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}
WORKDIR=${WORKDIR:-/opt/cloudpivot/backend}
DB=${DB:-cp_e2efix6}
PORT=${PORT:-18420}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
LOG=${LOG:-/root/cp-logs/e2efix6_instance.log}
PID=${PID:-/root/cp-logs/e2efix6_instance.pid}
mkdir -p "$(dirname "$LOG")"

case "${1:-start}" in
  start)
    if [ -f "$PID" ] && kill -0 "$(cat "$PID")" 2>/dev/null; then
      echo "already running pid=$(cat "$PID")"; exit 0
    fi
    if [ ! -x "$BIN" ]; then echo "FATAL: 二进制不存在：$BIN"; exit 1; fi
    cd "$WORKDIR" || exit 1
    export NIK_PROFILE=test
    export NIK_DATABASE_NAME="$DB"
    export NIK_DATABASE_USER=alun
    export NIK_DATABASE_PASSWORD=alun
    export NIK_DATABASE_HOST=127.0.0.1
    export NIK_DATABASE_PORT=5432
    export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
    export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
    export NIK_SEED_DEMO_USERS=true
    export NIK_MIDDLEWARE_AUTH_ENABLED=true
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
    export X_CONFIG_SECRET_KEY="$(openssl rand -hex 16)"
    export RUST_LOG=info
    setsid nohup "$BIN" > "$LOG" 2>&1 &
    echo $! > "$PID"
    echo "started pid=$(cat "$PID") db=$DB port=$PORT bin=$BIN log=$LOG"
    ;;
  status)
    if [ -f "$PID" ] && kill -0 "$(cat "$PID")" 2>/dev/null; then
      echo "running pid=$(cat "$PID")"
      curl -s -o /dev/null -w "health=%{http_code}\n" "http://127.0.0.1:$PORT/health"
    else
      echo "not running"
    fi
    ;;
  logs) tail -n "${2:-40}" "$LOG" ;;
  stop)
    if [ -f "$PID" ]; then
      kill "$(cat "$PID")" 2>/dev/null
      sleep 2
      kill -9 "$(cat "$PID")" 2>/dev/null
      rm -f "$PID"
      echo stopped
    else
      echo "no pid file"
    fi
    ;;
esac
