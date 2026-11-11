#!/usr/bin/env bash
# COST-B4-2 反查验收：在**演练库**上手动起一个实例（演练脚本跑完会停实例，但 --keep 留库）。
#
#   bash 02_up_instance.sh          # 起 :18142（库 cost_b42_e2e）
#   bash 02_up_instance.sh stop     # 停
#
# 只碰库名含 e2e 的演练库；三把密钥现场生成（一次性，不落盘）。
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DB="${DB:-cost_b42_e2e}"
PORT="${PORT:-18142}"
PIDFILE="/tmp/${DB}.ui.pid"
LOG="/tmp/${DB}.ui.log"

case "$DB" in
    *drill*|*e2e*|*test*) ;;
    *) echo "拒绝执行：库名 $DB 不像演练库" >&2; exit 2 ;;
esac

stop() {
    if [ -f "$PIDFILE" ]; then
        pid="$(cat "$PIDFILE")"
        kill "$pid" 2>/dev/null
        for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
        kill -9 "$pid" 2>/dev/null
        mv -f "$PIDFILE" "$PIDFILE.done" 2>/dev/null || true
        echo "已停 pid=$pid"
    fi
    port_pid="$(ss -ltnp 2>/dev/null | grep -F ":$PORT " | sed 's/.*pid=\([0-9]*\).*/\1/' | head -1)"
    if [ -n "$port_pid" ]; then kill -9 "$port_pid" 2>/dev/null; echo "已停占用 :$PORT 的 pid=$port_pid"; fi
}

if [ "${1:-up}" = "stop" ]; then stop; exit 0; fi

stop

: "${NIK_MIDDLEWARE_AUTH_JWT_SECRET:=$(head -c 48 /dev/urandom | base64 | tr -d '\n')}"
: "${CLOUDPIVOT_STORAGE_MASTER_KEY:=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')}"
: "${X_CONFIG_SECRET_KEY:=$(head -c 24 /dev/urandom | base64 | tr -d '\n')}"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET CLOUDPIVOT_STORAGE_MASTER_KEY X_CONFIG_SECRET_KEY
export NIK_PROFILE=test NIK_ENV=test
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_DATABASE_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
export NIK_DATABASE_USER="${NIK_DATABASE_USER:-alun}"
export NIK_DATABASE_PASSWORD="${NIK_DATABASE_PASSWORD:-alun}"
export NIK_DATABASE_NAME="$DB"

# 必须从 backend/ 起（config/ 是相对工作目录解析的）
( cd "$ROOT/backend" && nohup ./target/debug/cloudpivot > "$LOG" 2>&1 & echo $! > "$PIDFILE" )
for _ in $(seq 1 90); do
    if curl -sf -m 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
        lane_pid="$(ss -ltnp 2>/dev/null | grep -F ":$PORT " | sed 's/.*pid=\([0-9]*\).*/\1/' | head -1)"
        [ -n "$lane_pid" ] && echo "$lane_pid" > "$PIDFILE"
        echo "实例就绪 http://127.0.0.1:$PORT/health（库 $DB，日志 $LOG）"
        exit 0
    fi
    sleep 1
done
echo "实例没起来，日志末尾：" >&2
tail -20 "$LOG" >&2
exit 1
