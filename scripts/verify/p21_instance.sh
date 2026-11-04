#!/usr/bin/env bash
# P2-1：起 / 停隔离实例（库 cloudpivot_p16md 的副本现场，端口 18099），真机验收用。
#
#   bash scripts/verify/p21_instance.sh start
#   bash scripts/verify/p21_instance.sh stop
#   bash scripts/verify/p21_instance.sh status
#
# 为什么复用 `cloudpivot_p16md`：本机 `alun` 角色没有 CREATEDB 权限（`rolcreatedb=f`），
# 起不了新的空库；这个库是上一张卡（P1-6 主数据导入）留下来的**迁移齐全的副本库**，
# 与生产同 schema，且**不含生产凭证**（biz_voucher 0 行），拿它做辅助核算的真机验收
# 不会碰任何真实账。验收脚本自带前缀清场，可重复跑。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=${REPO:-/opt/cloudpivot}
DB=${P21_DB:-cloudpivot_p16md}
PORT=${P21_PORT:-18099}
LOG=${P21_LOG:-/root/p21-e2e-server.log}
PIDFILE=${P21_PIDFILE:-/root/p21-e2e-server.pid}

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
      return 0
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "进程已退出，看 $LOG"; return 1; }
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
