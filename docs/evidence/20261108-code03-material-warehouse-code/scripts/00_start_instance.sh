#!/bin/bash
# CODE-03 隔离实例启动（99 本机，profile=test，库 cloudpivot_code03，端口 127.0.0.1:8043）
#
# 与 prod(8023) / 145 test(8033) 完全隔离：独立库 + 独立端口 + 临时密钥（进程退出即失效）。
# 用法：bash 00_start_instance.sh start|stop|status
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

APP=/opt/cloudpivot/backend
DB=${DB:-cloudpivot_code03}
PORT=${PORT:-8043}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
LOG=/root/.hermes/profiles/backend-developer/cache/scratch/code03/instance.log
PID=/root/.hermes/profiles/backend-developer/cache/scratch/code03/instance.pid
mkdir -p "$(dirname "$LOG")"

case "${1:-start}" in
  start)
    if [ -f "$PID" ] && kill -0 "$(cat "$PID")" 2>/dev/null; then
      echo "already running pid=$(cat "$PID")"; exit 0
    fi
    cd "$APP"
    # 三把密钥：JWT（≥44 字符）/ 存储主密钥（64 位十六进制）/ 配置密文密钥
    export NIK_PROFILE=test
    export NIK_DATABASE_NAME="$DB"
    export NIK_DATABASE_USER=alun
    export NIK_DATABASE_PASSWORD=alun
    export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
    # 初始超管口令：只在**建 admin 那一次**生效（库里已有 admin 时 ensure_admin_user 不动它），
    # 不注入就会随机生成 24 位口令且不落日志 ⇒ curl 登录必然 10005/20001。
    export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
    export X_CONFIG_SECRET_KEY="$(openssl rand -hex 16)"
    setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 &
    echo $! > "$PID"
    echo "started pid=$(cat "$PID") db=$DB port=$PORT log=$LOG"
    ;;
  stop)
    if [ -f "$PID" ]; then
      kill "$(cat "$PID")" 2>/dev/null || true
      rm -f "$PID"
      echo "stopped"
    else
      echo "no pid file"
    fi
    ;;
  status)
    curl -s -o /dev/null -w "health=%{http_code}\n" "http://127.0.0.1:$PORT/health"
    ;;
  *) echo "usage: $0 start|stop|status"; exit 2;;
esac
