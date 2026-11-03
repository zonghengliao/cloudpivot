#!/usr/bin/env bash
# D3（t_217a95ad）：起 / 停隔离实例（库 cloudpivot_d3cfg，端口 18096），真机验收用。
#
#   bash scripts/verify/d3cfg_instance.sh start
#   bash scripts/verify/d3cfg_instance.sh stop
#   bash scripts/verify/d3cfg_instance.sh status
#
# 库用「干净库 + 全量迁移」（`d1_dbsetup.sh` 同款），所以启动日志里的
# 「数据库迁移完成：应用 N 个迁移」本身就是「干净库装得起来 + V0206 自检通过」的证据。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=/opt/cloudpivot
DB=${D3CFG_DB:-cloudpivot_d3cfg}
PORT=${D3CFG_PORT:-18096}
LOG=${D3CFG_LOG:-/root/d3cfg-e2e-server.log}
PIDFILE=${D3CFG_PIDFILE:-/root/d3cfg-e2e-server.pid}

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
      grep -m3 -E "V0206|flow_edge_config" "$LOG" || true
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
