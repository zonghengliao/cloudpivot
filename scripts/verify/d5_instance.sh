#!/usr/bin/env bash
# D5 隔离实例（库 cloudpivot_d5，端口 18095）：起 / 停
#   bash scripts/verify/d5_instance.sh up     # 起（迁移已跑过，日志会显示「应用 0 个迁移」）
#   bash scripts/verify/d5_instance.sh down   # 按 pid 停（不用 pkill，免得误杀别人的实例）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=cloudpivot_d5
PORT=18095
LOG=/tmp/d5_server.log
PIDFILE=/tmp/d5_server.pid
CMD=${1:-up}

case "$CMD" in
  up)
    if [ -f "$PIDFILE" ] && kill -0 "$(cat $PIDFILE)" 2>/dev/null; then
      echo "已在运行：pid $(cat $PIDFILE)"; exit 0
    fi
    cd /opt/cloudpivot/backend
    NIK_PROFILE=test \
    NIK_DATABASE_NAME=$DB \
    NIK_DATABASE_USER=alun \
    NIK_DATABASE_PASSWORD=alun \
    NIK_SERVER_LISTEN=127.0.0.1:$PORT \
    NIK_ADMIN_PASSWORD='Admin@123456' \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
    CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
    setsid nohup ./target/debug/cloudpivot > $LOG 2>&1 < /dev/null &
    echo $! > $PIDFILE
    for i in $(seq 1 60); do
      code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:$PORT/health || true)
      [ "$code" = "200" ] && { echo "up: pid $(cat $PIDFILE) health=200"; exit 0; }
      sleep 2
    done
    echo "启动失败，日志尾部："; tail -20 $LOG; exit 1
    ;;
  down)
    if [ -f "$PIDFILE" ]; then
      kill "$(cat $PIDFILE)" 2>/dev/null
      sleep 2
      kill -9 "$(cat $PIDFILE)" 2>/dev/null
      rm -f "$PIDFILE"
      echo "stopped"
    else
      echo "没有 pid 文件"
    fi
    ;;
  *)
    echo "用法：$0 up|down"; exit 2 ;;
esac
