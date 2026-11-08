#!/bin/bash
# CODE-09 端到端验证隔离实例（99 本机）：profile=test，库 cloudpivot_code09（**全新空库**），端口 127.0.0.1:8049
#
# 为什么空库：验证矩阵里的「编码传空 → 自动生成 PREFIX-0001 / WH-01 / AC-01 / ET-01」只有在
# 没有任何历史编码占号的库里才断言得干净（首号就是 0001/01）。
#
# 二进制来自 **隔离 worktree** /root/cp-code09-wt（detached HEAD = origin/main 6364972），
# 不用共享工作区：那边有自己的 WIP（asset_compute / invoice / payable 等未提交改动），
# 端到端证据必须钉在干净的 origin/main 上。
#   cargo build 走 `CARGO_TARGET_DIR=/opt/cloudpivot/backend/target` 复用已编译的依赖
#   （target 目录 91G，不能重建一份）。
#
# 用法：bash 00_start_instance.sh start|status|stop
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

BIN=${BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}
# app 按 **CWD** 读 `config/` 与 `migrations/`，所以必须在 worktree 的 backend 目录里启动
# （config-test.toml 不入库，已从共享工作区拷入）
WORKDIR=${WORKDIR:-/root/cp-code09-wt/backend}
DB=${DB:-cloudpivot_code09}
PORT=${PORT:-8049}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
RAW="$HERE/../raw"
SCRATCH=/root/.hermes/profiles/backend-developer/cache/scratch/code09
LOG=${LOG:-$SCRATCH/code09_instance.log}
PID=${PID:-$SCRATCH/code09_instance.pid}
mkdir -p "$(dirname "$LOG")" "$RAW"

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
    echo "started pid=$(cat "$PID") db=$DB port=$PORT bin=$BIN cwd=$WORKDIR log=$LOG"
    ;;
  status)
    curl -s -o /dev/null -w "health=%{http_code}\n" "http://127.0.0.1:$PORT/health"
    ;;
  stop)
    if [ -f "$PID" ]; then kill "$(cat "$PID")" 2>/dev/null || true; rm -f "$PID"; echo stopped; else echo "no pid file"; fi
    ;;
  *) echo "usage: $0 start|status|stop"; exit 2 ;;
esac
