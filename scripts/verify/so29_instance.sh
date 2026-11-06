#!/usr/bin/env bash
# =============================================================================
# SO-29 隔离实例：cloudpivot_so29 → 127.0.0.1:18229（真跑迁移 + 真后端）
# =============================================================================
#   bash scripts/verify/so29_db_setup.sh         # 先建空库
#   bash scripts/verify/so29_instance.sh up      # 起实例（启动日志 = 全量迁移 APPLY_OK）
#   bash scripts/verify/so29_instance.sh down    # 停实例
#   bash scripts/verify/so29_instance.sh log     # 看日志尾部
#
# 端口 18229 ∈ 隔离段（18227=SO-27 / 18228=SO-28），不撞邻居卡。
# 三把密钥必须显式注入：profile=test 下缺 JWT / 存储主密钥 / 配置密钥，
# 安全守卫直接 FATAL，迁移一条都不会跑（见 skill §6.48）。
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
DB=${SO29_DB:-cloudpivot_so29}
PORT=${SO29_PORT:-18229}
LOG=${SO29_LOG:-/root/so29_server.log}
PIDF=${SO29_PIDF:-/root/so29_server.pid}
BIN=${SO29_BIN:-$ROOT/backend/target/debug/cloudpivot}

stop_instance() {
  [ -f "$PIDF" ] || return 0
  kill "$(cat "$PIDF")" 2>/dev/null || true
  sleep 2
  kill -9 "$(cat "$PIDF")" 2>/dev/null || true
  rm -f "$PIDF"
}

case "${1:-up}" in
  down) stop_instance; echo "==> stopped"; exit 0 ;;
  log) tail -60 "$LOG"; exit 0 ;;
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
X_CONFIG_SECRET_KEY=$(openssl rand -hex 32) \
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDF"
echo "==> 启动 pid=$(cat "$PIDF") 库=$DB 端口=$PORT 日志=$LOG"

code=""
for _ in $(seq 1 120); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && break
  kill -0 "$(cat "$PIDF")" 2>/dev/null || { echo "FATAL: 实例提前退出"; tail -40 "$LOG"; exit 1; }
  sleep 2
done
echo "health=$code pid=$(cat "$PIDF")"
echo "==> 迁移日志"
grep -E "迁移|migration" "$LOG" | tail -8 || true
[ "$code" = "200" ] || { echo "FATAL: /health 未就绪"; tail -40 "$LOG"; exit 1; }
