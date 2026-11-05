#!/usr/bin/env bash
# P1-FIX-4：起 / 停隔离实例（库 cloudpivot_p1fix4，端口 18119）—— 真机验收用，不动生产 144。
#
#   bash docs/evidence/20261105-p1fix4-state-edge/p1fix4_instance.sh start
#   P1FIX4_BIN=/root/p1fix4-before-bin bash docs/evidence/20261105-p1fix4-state-edge/p1fix4_instance.sh start
#   bash docs/evidence/20261105-p1fix4-state-edge/p1fix4_instance.sh stop
#
# 口径与 docs/evidence/20261105-p1fix3/p1fix3_instance.sh 一致：
#   - profile=test（迁移自动应用；库已由 scripts/verify/erp_clean_db.sh 迁到最新）
#   - setsid 后台，按 pid 文件杀（别用 pkill -f，会误杀别人的实例）
#   - 必须等 /health 200（bootstrap 的种子 admin / SUPER_ADMIN 在迁移之后才种完）
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/bin:$PATH

ROOT=/opt/cloudpivot
DB=${P1FIX4_DB:-cloudpivot_p1fix4}
PORT=${P1FIX4_PORT:-18119}
BIN=${P1FIX4_BIN:-$ROOT/backend/target/debug/cloudpivot}
TAG=$(basename "$BIN" | tr -cd '[:alnum:]')
LOG=/root/p1fix4-instance-$DB-$TAG.log
PIDFILE=/root/p1fix4-instance-$DB-$TAG.pid

start() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "已在运行 pid=$(cat "$PIDFILE") bin=$BIN"; return 0
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
  setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDFILE"
  echo "启动 pid=$(cat "$PIDFILE") 库=$DB 端口=$PORT 二进制=$BIN"
  for _ in $(seq 1 240); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
    if [ "$code" = "200" ]; then
      echo "健康检查通过（/health 200）"
      grep -m1 "数据库迁移完成" "$LOG" || true
      return 0
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "进程已退出，看 $LOG"; tail -30 "$LOG"; return 1; }
    sleep 1
  done
  echo "健康检查超时"; tail -30 "$LOG"; return 1
}

stop() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    kill "$(cat "$PIDFILE")"; sleep 2
    kill -9 "$(cat "$PIDFILE")" 2>/dev/null || true
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
      echo "running pid=$(cat "$PIDFILE") bin=$BIN"
      curl -s -o /dev/null -w 'health=%{http_code}\n' "http://127.0.0.1:$PORT/health"
    else
      echo "stopped"
    fi
    ;;
  *) echo "用法：$0 {start|stop|status}" ;;
esac
