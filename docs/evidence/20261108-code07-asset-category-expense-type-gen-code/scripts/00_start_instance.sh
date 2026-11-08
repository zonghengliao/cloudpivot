#!/bin/bash
# CODE-07 隔离实例（99 本机）：profile=test，库 cloudpivot_code07e2e（**全新空库**），端口 127.0.0.1:8047
#
# 为什么要空库：本卡验收的是「点『生成编码』先看号、保存时用该号」，空库第一个号就是
# AC-01 / ET-01（biz_seq 的 AC / ET 号段 pad_width=2），断言「预览号 == 保存拿到的号」
# 最干净；顺带验证「全新库首启自动应用全部迁移」这条幂等性。
#
# 二进制来自**隔离 worktree** `/root/cp-code07-wt`（detached HEAD 506fa34 = 本卡开工时的
# origin/main），与共享工作区里别人未提交的后端改动完全隔离（那些改动不归本卡，且编译不过）。
#
# 用法：bash 00_start_instance.sh start|status|stop
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

BIN=${BIN:-/root/cp-code07-wt/backend/target/debug/cloudpivot}
# app 按 **CWD** 读 `config/` 与 `migrations/`，所以必须在 backend 目录里启动。
WORKDIR=${WORKDIR:-/root/cp-code07-wt/backend}
DB=${DB:-cloudpivot_code07e2e}
PORT=${PORT:-8047}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
LOG=${LOG:-/root/cp-logs/code07_instance.log}
PID=${PID:-/root/cp-logs/code07_instance.pid}

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
    # 仓内 `config/config.toml` 的 `[middleware.auth] enabled = false`（基础配置保持关闭，
    # 由各环境 profile 开启）；本机没有 `config/config-test.toml`，所以必须显式打开，
    # 否则认证层不挂载 ⇒ 所有 `#[nik::permission]` 接口回 403「无权限访问」。
    export NIK_MIDDLEWARE_AUTH_ENABLED=true
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
