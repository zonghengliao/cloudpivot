#!/bin/bash
# CODE-04 隔离实例（99 本机）：profile=test，库 cloudpivot_code04，端口 127.0.0.1:8044
#
# 为什么不用 145 的常驻实例：145 是共享 dev 环境，重启要靠 `systemctl restart cloudpivot-test`
# （无人值守的 Agent 会话不允许动 systemd 服务）；145 侧本卡只做 `git pull` + `cargo build`
# （已留证）。真实 token curl 跑在本脚本拉起的**同 commit、同二进制**的隔离实例上，
# 库是从 145 同版本迁移全量灌出来的独立库（cloudpivot_code04），互不干扰。
#
# 用法：bash 00_start_instance.sh start|status|stop
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

APP=/opt/cloudpivot/backend
DB=${DB:-cloudpivot_code04}
PORT=${PORT:-8044}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
LOG=${LOG:-/root/.hermes/profiles/backend-developer/cache/scratch/code04/instance.log}
PID=${PID:-/root/.hermes/profiles/backend-developer/cache/scratch/code04/instance.pid}
mkdir -p "$(dirname "$LOG")"

case "${1:-start}" in
  start)
    if [ -f "$PID" ] && kill -0 "$(cat "$PID")" 2>/dev/null; then
      echo "already running pid=$(cat "$PID")"; exit 0
    fi
    cd "$APP" || exit 1
    export NIK_PROFILE=test
    export NIK_DATABASE_NAME="$DB"
    export NIK_DATABASE_USER=alun
    export NIK_DATABASE_PASSWORD=alun
    export NIK_DATABASE_HOST=127.0.0.1
    export NIK_DATABASE_PORT=5432
    export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
    export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
    export X_CONFIG_SECRET_KEY="$(openssl rand -hex 16)"
    export RUST_LOG=info
    setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 &
    echo $! > "$PID"
    echo "started pid=$(cat "$PID") db=$DB port=$PORT log=$LOG"
    ;;
  status)
    curl -s -o /dev/null -w "health=%{http_code}\n" "http://127.0.0.1:$PORT/health"
    ;;
  stop)
    if [ -f "$PID" ]; then kill "$(cat "$PID")" 2>/dev/null || true; rm -f "$PID"; echo stopped; else echo "no pid file"; fi
    ;;
  *) echo "usage: $0 start|status|stop"; exit 2 ;;
esac
