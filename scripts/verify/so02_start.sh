#!/usr/bin/env bash
# SO-02 隔离实例：把 cloudpivot_v0260 起到 127.0.0.1:18097（应用 V0261 并 bootstrap）
#
# 用法：bash scripts/verify/so02_start.sh start|stop
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

REPO=${REPO:-/opt/cloudpivot}
DB=${ERP_DB:-cloudpivot_v0260}
PORT=${PORT:-18097}
PGHOST=${PGHOST:-127.0.0.1}
PGPORT=${PGPORT:-5432}
DB_USER=${DB_USER:-alun}
DB_PASS=${DB_PASS:-alun}
BIN=${BIN:-$REPO/backend/target/debug/cloudpivot}
LOG=${LOG:-/root/so02-server.log}
PIDFILE=${PIDFILE:-/root/so02-server.pid}

stop() {
    if [ -f "$PIDFILE" ]; then
        kill "$(cat "$PIDFILE")" 2>/dev/null
        sleep 1
        kill -9 "$(cat "$PIDFILE")" 2>/dev/null
        rm -f "$PIDFILE"
        echo "==> stopped"
    fi
}

case "${1:-start}" in
    stop) stop; exit 0 ;;
esac

stop
export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST="$PGHOST"
export NIK_DATABASE_PORT="$PGPORT"
export NIK_DATABASE_USER="$DB_USER"
export NIK_DATABASE_PASSWORD="$DB_PASS"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET=${NIK_MIDDLEWARE_AUTH_JWT_SECRET:-$(openssl rand -hex 32)}
export CLOUDPIVOT_STORAGE_MASTER_KEY=${CLOUDPIVOT_STORAGE_MASTER_KEY:-$(openssl rand -hex 32)}
export NIK_ADMIN_PASSWORD=${NIK_ADMIN_PASSWORD:-Admin@123456}

: > "$LOG"
cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"
echo "==> 启动 pid=$(cat "$PIDFILE") 库=$DB 端口=$PORT 日志=$LOG"

for _ in $(seq 1 180); do
    if grep -q "数据库迁移完成" "$LOG" 2>/dev/null; then
        echo "==> 迁移完成"
        grep "数据库迁移完成" "$LOG" | tail -2
        break
    fi
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || { echo "!! 进程已退出"; tail -20 "$LOG"; exit 1; }
    sleep 1
done

# 等 HTTP 真的能应答（/health 免前缀）
for _ in $(seq 1 60); do
    code=$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null)
    [ "$code" = "200" ] && { echo "==> /health 200"; break; }
    sleep 1
done
echo "==> 启动日志尾部"; tail -5 "$LOG"
