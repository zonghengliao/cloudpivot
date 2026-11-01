#!/usr/bin/env bash
# D9 隔离实例（cloudpivot_d9 + 127.0.0.1:18096）：起实例 → 跑真机 e2e → 可选停实例
#   bash scripts/verify/d9_instance.sh up     # 起实例并等 /health
#   bash scripts/verify/d9_instance.sh e2e    # 跑 d9_document_copy_e2e.py
#   bash scripts/verify/d9_instance.sh down   # 按 pid 停实例
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=cloudpivot_d9
PORT=18096
LOG=/tmp/d9_e2e_server.log
PIDF=/tmp/d9_e2e_server.pid
ACTION="${1:-up}"

case "$ACTION" in
  up)
    cd /opt/cloudpivot/backend
    cargo build --lib --bin cloudpivot 2>&1 | tail -2
    kill "$(cat $PIDF 2>/dev/null)" 2>/dev/null || true
    sleep 1
    rm -f "$PIDF"
    NIK_PROFILE=test \
    NIK_DATABASE_NAME=$DB \
    NIK_DATABASE_USER=alun \
    NIK_DATABASE_PASSWORD=alun \
    NIK_SERVER_LISTEN=127.0.0.1:$PORT \
    NIK_ADMIN_PASSWORD='Admin@123456' \
    NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
    CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
    setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
    echo $! > "$PIDF"
    for i in $(seq 1 90); do
      code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
      if [ "$code" = "200" ]; then break; fi
      if ! kill -0 "$(cat $PIDF)" 2>/dev/null; then echo "实例提前退出"; tail -20 "$LOG"; exit 1; fi
      sleep 2
    done
    echo "health=$code pid=$(cat $PIDF)"
    grep -E "数据库迁移完成" "$LOG" | tail -1
    ;;
  e2e)
    cd /opt/cloudpivot
    D9_BASE_URL="http://127.0.0.1:$PORT" D9_DB_NAME="$DB" \
      python3 scripts/verify/d9_document_copy_e2e.py
    ;;
  down)
    kill "$(cat $PIDF 2>/dev/null)" 2>/dev/null || true
    sleep 3
    kill -9 "$(cat $PIDF 2>/dev/null)" 2>/dev/null || true
    echo "stopped"
    ;;
  *)
    echo "用法：$0 {up|e2e|down}"
    exit 2
    ;;
esac
