#!/usr/bin/env bash
# =============================================================================
# SO-26 隔离实例：cloudpivot_so26 起到 127.0.0.1:18226（真跑迁移 + 真后端）
# =============================================================================
#   bash scripts/verify/so26_instance.sh up     # 起实例（库须已存在，见 README）
#   bash scripts/verify/so26_instance.sh down   # 停实例
#   bash scripts/verify/so26_instance.sh log    # 看日志尾部
#
# 库 cloudpivot_so26 = 从 cloudpivot_so25（227 条迁移的干净装配库）TEMPLATE 复制。
# 端口 18226 ∈ 隔离段 18000-18999（18225=SO-25），不与邻居卡撞。
# 本卡**无表结构改动** ⇒ 启动时应「应用 0 个迁移」，这正是卡面第 4 条的证据。
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
DB=${SO26_DB:-cloudpivot_so26}
PORT=${SO26_PORT:-18226}
LOG=${SO26_LOG:-/root/so26_server.log}
PIDF=${SO26_PIDF:-/root/so26_server.pid}
BIN=${SO26_BIN:-$ROOT/backend/target/debug/cloudpivot}

stop_instance() {
  [ -f "$PIDF" ] || return 0
  kill "$(cat "$PIDF")" 2>/dev/null || true
  sleep 2
  kill -9 "$(cat "$PIDF")" 2>/dev/null || true
  rm -f "$PIDF"
}

case "${1:-up}" in
  down) stop_instance; echo "==> stopped"; exit 0 ;;
  log) tail -40 "$LOG"; exit 0 ;;
esac

[ -x "$BIN" ] || { echo "FATAL: 找不到二进制 $BIN（先 cargo build）"; exit 1; }
stop_instance

cd "$ROOT/backend" || exit 1
NIK_PROFILE=test \
NIK_DATABASE_NAME="$DB" \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDF"
echo "==> 启动 pid=$(cat "$PIDF") 库=$DB 端口=$PORT 日志=$LOG"

code=""
for _ in $(seq 1 90); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && break
  kill -0 "$(cat "$PIDF")" 2>/dev/null || { echo "FATAL: 实例提前退出"; tail -40 "$LOG"; exit 1; }
  sleep 2
done
echo "health=$code pid=$(cat "$PIDF")"
echo "==> 迁移日志"
grep -E "迁移|migration" "$LOG" | tail -8 || true
[ "$code" = "200" ] || { echo "FATAL: /health 未就绪"; tail -40 "$LOG"; exit 1; }
