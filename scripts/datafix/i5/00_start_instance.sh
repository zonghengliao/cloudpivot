#!/usr/bin/env bash
# I-5 · 在 145 上用 cloudpivot_dev_clean 起一个隔离实例（默认 :18059）
#   - 库/端口可覆盖：DB=... PORT=... bash 00_start_instance.sh
#   - 起完会核对端口监听 pid 就是自己起的进程（上一张卡踩过「打到别人实例」的坑）
set -uo pipefail
DB=${DB:-cloudpivot_dev_clean}
PORT=${PORT:-18059}
ENV_FILE=${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}
BIN=${BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}
LOG=${LOG:-/tmp/i5/clean_smoke_instance.log}
PIDFILE=${PIDFILE:-/tmp/i5/clean_smoke_instance.pid}
mkdir -p "$(dirname "$PIDFILE")"

set -a; . "$ENV_FILE"; set +a
export NIK_DATABASE_NAME=$DB NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:$PORT RUST_LOG=info
unset NIK_REDIS_URL   # 隔离实例不共用 Redis（避免和其它演练实例互相干扰）

[ -f "$PIDFILE" ] && kill "$(cat "$PIDFILE")" 2>/dev/null && sleep 1
cd /opt/cloudpivot/backend || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"
code=000
for _ in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$PORT/health" 2>/dev/null)
  [ "$code" = "200" ] && break
  sleep 1
done
listener=$(ss -ltnp "sport = :$PORT" 2>/dev/null | grep -o "pid=[0-9]*" | head -1)
echo "pid=$(cat "$PIDFILE") health=$code listener=$listener"
[ "$listener" = "pid=$(cat "$PIDFILE")" ] || { echo "❌ 端口 $PORT 的监听者不是本进程（$listener），停止" >&2; exit 1; }
