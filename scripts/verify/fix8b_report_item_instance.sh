#!/usr/bin/env bash
# P1-FIX-8B：起 / 停隔离实例（库 cloudpivot_ri8，端口 18160）。真机验收用。
#
#   bash scripts/verify/fix8b_report_item_instance.sh start
#   bash scripts/verify/fix8b_report_item_instance.sh stop
#   bash scripts/verify/fix8b_report_item_instance.sh status
#
# 二进制：/root/p1fix5c-target/debug/cloudpivot（本卡改动编出来的），不碰生产 / backend/target。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=/opt/cloudpivot
BIN=${BIN:-/root/p1fix5c-target/debug/cloudpivot}
DB=${DB:-cloudpivot_ri8}
PORT=${PORT:-18160}
LOG=/root/fix8b-report-item-server.log
PIDFILE=/root/fix8b-report-item-server.pid

start() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "已在运行 pid=$(cat "$PIDFILE")"
    return 0
  fi
  [ -x "$BIN" ] || { echo "二进制不存在：$BIN（先 cargo build --bin cloudpivot）"; return 1; }
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
  setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDFILE"
  echo "启动 pid=$(cat "$PIDFILE") 库=$DB 端口=$PORT 日志=$LOG"
  for _ in $(seq 1 240); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
    if [ "$code" = "200" ]; then
      echo "健康检查通过（/health 200）"
      grep -m5 "V0256\|报表项维护" "$LOG" || true
      return 0
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "进程已退出，看 $LOG"; tail -30 "$LOG"; return 1; }
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
    echo "已停止"
  else
    echo "未在运行"
  fi
  rm -f "$PIDFILE"
}

status() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "running pid=$(cat "$PIDFILE")"
    curl -s -o /dev/null -w 'health=%{http_code}\n' "http://127.0.0.1:$PORT/health" || true
  else
    echo "stopped"
  fi
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  status) status ;;
  *) echo "用法: $0 {start|stop|status}"; exit 1 ;;
esac
