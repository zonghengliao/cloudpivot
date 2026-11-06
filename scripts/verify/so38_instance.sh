#!/usr/bin/env bash
# SO-38 隔离实例（库 cloudpivot_so38，端口 18245）：起 / 停
#   bash scripts/verify/so38_instance.sh up     # 起（启动时自动跑 pending 迁移：V0313）
#   bash scripts/verify/so38_instance.sh down   # 按 pid 停
#
# 库来源：`cloudpivot_so37` 模板克隆（已过 V0309–V0312：佣金三表 + 规则/结算接口 + 菜单页），
# 本卡只多跑 V0313（客户 credit_used 列 + 额度台账 + 重算任务行）。
#
# 二进制：CARGO_TARGET_DIR=/root/cargo-target-shared（避免与其它卡抢 /tmp 与 backend/target 锁）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=${SO38_DB:-cloudpivot_so38}
PORT=${SO38_PORT:-18245}
LOG=${SO38_LOG:-/root/so38_server.log}
PIDFILE=${SO38_PID:-/root/so38_server.pid}
BIN=${SO38_BIN:-/root/cargo-target-shared/debug/cloudpivot}
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
    setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
    echo $! > "$PIDFILE"
    for i in $(seq 1 60); do
      code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
      [ "$code" = "200" ] && { echo "up: pid $(cat $PIDFILE) health=200 port=$PORT db=$DB"; exit 0; }
      sleep 2
    done
    echo "启动失败，日志尾部："
    tail -40 "$LOG"
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
    exit 1
    ;;
esac
