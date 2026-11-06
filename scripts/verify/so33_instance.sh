#!/usr/bin/env bash
# SO-33 取价解析隔离实例（库 cloudpivot_so33，端口 18239）：起 / 停
#   bash scripts/verify/so33_instance.sh up     # 起（启动时自动跑 pending 迁移：V0307 权限码）
#   bash scripts/verify/so33_instance.sh down   # 按 pid 停（不用 pkill，免得误杀别人的实例）
#
# 与 scripts/verify/d5_instance.sh 同一套口子：
#   · profile=test 走 backend/config/config-test.toml；
#   · 三把密钥一律显式注入（配置文件里不许出现密钥）；
#   · 库名 / 端口用环境变量覆盖，绝不连开发库 cloudpivot。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=${SO33_DB:-cloudpivot_so33}
PORT=${SO33_PORT:-18239}
LOG=/tmp/so33_server.log
PIDFILE=/tmp/so33_server.pid
CMD=${1:-up}

case "$CMD" in
  up)
    if [ -f "$PIDFILE" ] && kill -0 "$(cat $PIDFILE)" 2>/dev/null; then
      echo "已在运行：pid $(cat $PIDFILE)"
      exit 0
    fi
    cd /opt/cloudpivot/backend || exit 1
    NIK_PROFILE=test \
    NIK_DATABASE_NAME=$DB \
    NIK_DATABASE_USER=alun \
    NIK_DATABASE_PASSWORD=alun \
    NIK_SERVER_LISTEN=127.0.0.1:$PORT \
    NIK_ADMIN_PASSWORD='Admin@123456' \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
    CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
    X_CONFIG_SECRET_KEY=$(openssl rand -hex 32) \
    setsid nohup ./target/debug/cloudpivot > $LOG 2>&1 < /dev/null &
    echo $! > $PIDFILE
    for i in $(seq 1 60); do
      code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:$PORT/health || true)
      [ "$code" = "200" ] && { echo "up: pid $(cat $PIDFILE) health=200"; exit 0; }
      sleep 2
    done
    echo "启动失败，日志尾部："
    tail -30 $LOG
    exit 1
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
    echo "用法：$0 up|down"
    exit 2
    ;;
esac
