#!/usr/bin/env bash
# P1-4 业务单据附件：起 / 停隔离实例（默认库 cp_p14_e2e，端口 18098），真机验收用。
#
#   bash scripts/verify/p14_instance.sh start
#   bash scripts/verify/p14_instance.sh stop
#   bash scripts/verify/p14_instance.sh status
#
# 空库起：迁移（含 V0212 单据附件）由启动链自动跑一遍 —— 这本身就是
# 「迁移能在真实启动路径上过一遍」的证据。
#
# 库名 / 端口可覆盖：P14_DB=xxx P14_PORT=18098 bash scripts/verify/p14_instance.sh start
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=${REPO:-/opt/cloudpivot}
DB=${P14_DB:-cp_p14_e2e}
PORT=${P14_PORT:-18098}
LOG=${P14_LOG:-/root/p14-e2e-server.log}
PIDFILE=${P14_PIDFILE:-/root/p14-e2e-server.pid}

start() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "已在运行 pid=$(cat "$PIDFILE")"
    return 0
  fi
  export NIK_PROFILE=test
  export NIK_DATABASE_NAME="$DB"
  export NIK_DATABASE_HOST=127.0.0.1
  export NIK_DATABASE_PORT=5432
  export NIK_DATABASE_USER=alun
  export NIK_DATABASE_PASSWORD=alun
  export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
  export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
  export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
  export NIK_ADMIN_PASSWORD='Admin@123456'
  rm -f "$LOG"
  cd "$ROOT/backend" || exit 1
  setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDFILE"
  echo "启动 pid=$(cat "$PIDFILE") 库=$DB 端口=$PORT 日志=$LOG"
  for _ in $(seq 1 180); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
    if [ "$code" = "200" ]; then
      echo "健康检查通过（/health 200）"
      grep -m1 "数据库迁移完成" "$LOG" || true
      grep -m1 "V0212" "$LOG" || true
      return 0
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "进程已退出，看 $LOG"; tail -20 "$LOG"; return 1; }
    sleep 1
  done
  echo "健康检查超时，看 $LOG"
  return 1
}

stop() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    kill "$(cat "$PIDFILE")"
    for _ in $(seq 1 30); do
      kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
      sleep 1
    done
    echo "已停 pid=$(cat "$PIDFILE")"
  else
    echo "没在运行"
  fi
  rm -f "$PIDFILE"
}

case "${1:-status}" in
  start) start ;;
  stop) stop ;;
  status)
    if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
      echo "running pid=$(cat "$PIDFILE")"
      curl -s -o /dev/null -w 'health=%{http_code}\n' "http://127.0.0.1:$PORT/health"
    else
      echo "stopped"
    fi
    ;;
  *) echo "用法：$0 {start|stop|status}" ;;
esac
