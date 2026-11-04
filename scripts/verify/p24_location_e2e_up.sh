#!/usr/bin/env bash
# P2-4 真机验收的隔离实例（起 / 停）—— 只碰本机测试库，不碰生产
#
#   bash scripts/verify/p24_location_e2e_up.sh up     # 起 :18096（库已迁移过就直接用）
#   bash scripts/verify/p24_location_e2e_up.sh down   # 停实例（**不删库**：验收证据留在库里）
#
# 与 dc03_e2e_up.sh 的差别：**不 dropdb**（库由 d1_dbsetup.sh + d1_migrate.sh 建好，
# 里面的数据就是验收证据）；只负责起 / 停进程。
set -uo pipefail
DB="${P24_DB:-cloudpivot_p24}"
PORT="${P24_PORT:-18096}"
BIN="${P24_BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}"
PIDF="/tmp/p24_e2e_${PORT}.pid"
LOG="/tmp/p24_e2e_${PORT}.log"

case "${1:-up}" in
up)
  if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; then
    echo "already running pid=$(cat "$PIDF")"; exit 0
  fi
  [ -x "$BIN" ] || { echo "缺少二进制：$BIN（先 cargo build）"; exit 1; }
  cd /opt/cloudpivot/backend || exit 1
  export NIK_PROFILE=test NIK_DATABASE_NAME="$DB" NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
         NIK_SERVER_LISTEN=127.0.0.1:"$PORT" NIK_ADMIN_PASSWORD='Admin@123456' \
         NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)" \
         CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
  setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDF"
  for i in $(seq 1 90); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
    [ "$code" = "200" ] && { echo "up pid=$(cat "$PIDF") health=200 after ${i}s db=$DB"; exit 0; }
    sleep 1
  done
  echo "FAILED to become healthy"; tail -5 "$LOG"; exit 1
  ;;
down)
  if [ -f "$PIDF" ]; then kill "$(cat "$PIDF")" 2>/dev/null; sleep 1; rm -f "$PIDF"; fi
  echo "down（库 $DB 保留，作为验收证据）"
  ;;
*)
  echo "usage: $0 up|down"; exit 2
  ;;
esac
