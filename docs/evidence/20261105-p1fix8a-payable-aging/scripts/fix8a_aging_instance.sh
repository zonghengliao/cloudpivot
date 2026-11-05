#!/usr/bin/env bash
# P1-FIX-8A：起 / 停隔离实例（库 cloudpivot_aging8，端口 18144）。真机验收用。
#
#   bash scripts/verify/fix8a_aging_instance.sh start
#   bash scripts/verify/fix8a_aging_instance.sh stop
#   bash scripts/verify/fix8a_aging_instance.sh status
#
# 二进制：/root/p1fix5c-target/debug/cloudpivot（本卡改动编出来的），不碰生产 / backend/target。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=/opt/cloudpivot
BIN=${BIN:-/root/p1fix5c-target/debug/cloudpivot}
DB=${DB:-cloudpivot_aging8}
PORT=${PORT:-18144}
LOG=/root/fix8a-aging-server.log
PIDFILE=/root/fix8a-aging-server.pid

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
  for _ in $(seq 1 180); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
    if [ "$code" = "200" ]; then
      echo "健康检查通过（/health 200）"
      grep -m3 "数据库迁移完成\|V0255\|应付账龄" "$LOG" || true
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
    rm -f "$PIDFILE"
    echo "已停"
  else
    echo "没在运行"
  fi
}

status() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "运行中 pid=$(cat "$PIDFILE") health=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")"
  else
    echo "没在运行"
  fi
}

case "${1:-start}" in
  start) start ;;
  stop) stop ;;
  status) status ;;
  *) echo "用法：$0 {start|stop|status}"; exit 2 ;;
esac
