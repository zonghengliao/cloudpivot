#!/usr/bin/env bash
# P1-4 前端截图用 dev server（5174 → 隔离实例 18098）。起 / 停各一次。
#   bash scripts/verify/p14_web.sh start|stop
set -uo pipefail
ROOT=${REPO:-/opt/cloudpivot}
PORT=${P14_WEB_PORT:-5174}
LOG=${P14_WEB_LOG:-/root/p14-web.log}
PIDFILE=${P14_WEB_PIDFILE:-/root/p14-web.pid}

start() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "已在运行 pid=$(cat "$PIDFILE")"
    return 0
  fi
  rm -f "$LOG"
  cd "$ROOT/frontend" || exit 1
  BACKEND_ORIGIN="http://127.0.0.1:${P14_BACKEND_PORT:-18098}" \
    setsid nohup pnpm dev --port "$PORT" --host 127.0.0.1 > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDFILE"
  echo "启动 dev server pid=$(cat "$PIDFILE") 端口=$PORT 日志=$LOG"
  for _ in $(seq 1 90); do
    if curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/" | grep -q 200; then
      echo "前端就绪 http://127.0.0.1:$PORT/"
      return 0
    fi
    sleep 1
  done
  echo "超时，看 $LOG"; tail -20 "$LOG"; return 1
}

stop() {
  if [ -f "$PIDFILE" ]; then
    kill "$(cat "$PIDFILE")" 2>/dev/null || true
    pkill -f "vite.*--port $PORT" 2>/dev/null || true
    rm -f "$PIDFILE"
    echo "已停"
  else
    echo "没在运行"
  fi
}

case "${1:-status}" in
  start) start ;;
  stop) stop ;;
  *) curl -s -o /dev/null -w "status=%{http_code}\n" "http://127.0.0.1:$PORT/" ;;
esac
