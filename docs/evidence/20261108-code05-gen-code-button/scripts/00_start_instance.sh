#!/bin/bash
# CODE-05 隔离实例（99 本机）：profile=test，库 cloudpivot_code05（**全新空库**），端口 127.0.0.1:8045
#
# 为什么要空库：本卡的核心是「预览不占号 + 保存时取号」，空库第一次自动号就是
# SUP-0001 / CUS-0001，断言「预览 == 保存拿到的号」最干净（不用猜历史占号）。
#
# 二进制来自**隔离 worktree**（HEAD 17389af + 本卡 2 个后端文件），
# 与共享工作区里别人未提交的 flow 子流程改动完全隔离（那些改动当时编译不过）。
#
# 用法：bash 00_start_instance.sh start|status|stop
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

BIN=${BIN:-/root/cp-code05-bin/cloudpivot}
# 二进制与配置/迁移分开放：app 按 **CWD** 读 `config/` 与 `migrations/`，
# 所以必须在 worktree 的 backend 目录里启动（那里有 config.toml + 263 个迁移）。
WORKDIR=${WORKDIR:-/root/cp-code05-wt/backend}
DB=${DB:-cloudpivot_code05a}
PORT=${PORT:-8045}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
LOG=${LOG:-/root/cp-logs/code05_instance.log}
PID=${PID:-/root/cp-logs/code05_instance.pid}

case "${1:-start}" in
  start)
    if [ -f "$PID" ] && kill -0 "$(cat "$PID")" 2>/dev/null; then
      echo "already running pid=$(cat "$PID")"; exit 0
    fi
    if [ ! -x "$BIN" ]; then echo "FATAL: 二进制不存在：$BIN"; exit 1; fi
    if [ ! -d "$WORKDIR" ]; then echo "FATAL: 工作目录不存在：$WORKDIR"; exit 1; fi
    cd "$WORKDIR" || exit 1
    export NIK_PROFILE=test
    export NIK_DATABASE_NAME="$DB"
    export NIK_DATABASE_USER=alun
    export NIK_DATABASE_PASSWORD=alun
    export NIK_DATABASE_HOST=127.0.0.1
    export NIK_DATABASE_PORT=5432
    export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
    export NIK_ADMIN_PASSWORD="$ADMIN_PASS"
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
    export X_CONFIG_SECRET_KEY="$(openssl rand -hex 16)"
    export RUST_LOG=info
    setsid nohup "$BIN" > "$LOG" 2>&1 &
    echo $! > "$PID"
    echo "started pid=$(cat "$PID") db=$DB port=$PORT bin=$BIN log=$LOG"
    ;;
  status)
    curl -s -o /dev/null -w "health=%{http_code}\n" "http://127.0.0.1:$PORT/health"
    ;;
  stop)
    if [ -f "$PID" ]; then kill "$(cat "$PID")" 2>/dev/null || true; rm -f "$PID"; echo stopped; else echo "no pid file"; fi
    ;;
  *) echo "usage: $0 start|status|stop"; exit 2 ;;
esac
