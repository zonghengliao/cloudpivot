#!/usr/bin/env bash
# FIN-3 隔离实例（库 cloudpivot_fin3 / 端口 18026），真机验收用。
#
#   bash scripts/verify/fin3_instance.sh start
#   bash scripts/verify/fin3_instance.sh stop
#   bash scripts/verify/fin3_instance.sh status
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=/opt/cloudpivot
DB=${FIN3_DB:-cloudpivot_fin3}
PORT=${FIN3_PORT:-18026}
LOG=/root/fin3-e2e-server.log
PIDFILE=/root/fin3-e2e-server.pid

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
  export TMPDIR=/root
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
      return 0
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "进程已退出，看 $LOG"; tail -30 "$LOG"; return 1; }
    sleep 1
  done
  echo "健康检查超时，看 $LOG"
  tail -30 "$LOG"
  return 1
}

stop() {
  if [ -f "$PIDFILE" ]; then
    pid=$(cat "$PIDFILE")
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 30); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.5
    done
    rm -f "$PIDFILE"
    echo "已停止 pid=$pid"
  else
    echo "没有 pid 文件"
  fi
}

status() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "运行中 pid=$(cat "$PIDFILE") 端口=$PORT 库=$DB"
    curl -s -o /dev/null -w 'health=%{http_code}\n' "http://127.0.0.1:$PORT/health"
  else
    echo "未运行"
  fi
}

case "${1:-start}" in
  start) start ;;
  stop) stop ;;
  status) status ;;
  *) echo "用法：$0 start|stop|status"; exit 2 ;;
esac
