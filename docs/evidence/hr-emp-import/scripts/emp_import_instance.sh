#!/usr/bin/env bash
# 员工档案导入（卡 t_b0808afe）隔离实例：库 cp_hr_emp_import / 端口 18089 / 非 prod。
# 用法：bash emp_import_instance.sh start|stop|status
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=/opt/cloudpivot
DB=${EMP_IMPORT_DB:-cp_hr_emp_import}
PORT=${EMP_IMPORT_PORT:-18089}
LOG=/root/emp-import-server.log
PIDFILE=/root/emp-import-server.pid

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
  export X_CONFIG_SECRET_KEY="$(openssl rand -hex 32)"
  export NIK_ADMIN_PASSWORD='Admin@123456'
  export TMPDIR=/root/tmp
  rm -f "$LOG"
  # 工作目录用 /root/emp-import-run：那里的 `config` 指向仓库配置，`migrations` 指向
  # 仓库外副本（system/V0397 与 business/V0397 撞号，另一张卡的 follow-up t_b4cc9e22
  # 已上报；本卡不改被跟踪文件，只把 system 那笔顺延到 V0399）。
  # 注意：`[database.migration] path` 是**相对 CWD** 的，且没有对应的 NIK_* 环境变量。
  cd /root/emp-import-run || exit 1
  setsid nohup "$ROOT/backend/target/debug/cloudpivot" > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDFILE"
  echo "启动 pid=$(cat "$PIDFILE") 库=$DB 端口=$PORT 日志=$LOG"
  for _ in $(seq 1 180); do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
    if [ "$code" = "200" ]; then
      echo "健康检查通过（/health 200）"
      grep -m3 -E "迁移|migration" "$LOG" || true
      return 0
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "进程已退出，看 $LOG"; tail -20 "$LOG"; return 1; }
    sleep 1
  done
  echo "健康检查超时，看 $LOG"
  tail -20 "$LOG"
  return 1
}

stop() {
  if [ -f "$PIDFILE" ]; then
    pid=$(cat "$PIDFILE")
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 30); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 1
    done
    kill -9 "$pid" 2>/dev/null || true
    rm -f "$PIDFILE"
    echo "已停止 pid=$pid"
  else
    echo "没有 pid 文件"
  fi
}

status() {
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    echo "运行中 pid=$(cat "$PIDFILE") 端口=$PORT 库=$DB"
  else
    echo "未运行"
  fi
}

case "${1:-start}" in
  start) start ;;
  stop) stop ;;
  status) status ;;
  *) echo "用法：$0 start|stop|status" ;;
esac
