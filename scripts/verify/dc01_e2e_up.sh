#!/usr/bin/env bash
# DC-01 真 HTTP 验收的隔离实例（起 / 停）—— 只碰本机 scratch 库，不碰生产
#
#   bash scripts/verify/dc01_e2e_up.sh up     # 建库 cp_dc01_e2e → 起 :18097 → 迁移 + 种子
#   bash scripts/verify/dc01_e2e_up.sh down   # 停实例 + dropdb
#
# 二进制默认取 `backend/target/debug/cloudpivot`，可用 DC01_BIN 覆盖。
set -uo pipefail
DB="${DC01_E2E_DB:-cp_dc01_e2e}"
PORT="${DC01_E2E_PORT:-18097}"
BIN="${DC01_BIN:-/opt/cloudpivot/backend/target/debug/cloudpivot}"
PIDF="/tmp/dc01_e2e_${PORT}.pid"
LOG="/tmp/dc01_e2e_${PORT}.log"

case "${1:-up}" in
up)
  if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; then
    echo "already running pid=$(cat "$PIDF")"; exit 0
  fi
  [ -x "$BIN" ] || { echo "缺少二进制：$BIN（先 cargo build）"; exit 1; }
  su postgres -c "psql -qc \"DROP DATABASE IF EXISTS $DB WITH (FORCE)\"" >/dev/null || exit 1
  su postgres -c "psql -qc \"CREATE DATABASE $DB OWNER alun\"" >/dev/null || exit 1
  cd /opt/cloudpivot/backend || exit 1
  export NIK_PROFILE=test NIK_DATABASE_NAME=$DB NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
         NIK_SERVER_LISTEN=127.0.0.1:$PORT NIK_ADMIN_PASSWORD='Admin@123456' \
         NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
         CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32)
  setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDF"
  for i in $(seq 1 120); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
    [ "$code" = "200" ] && { echo "up pid=$(cat "$PIDF") health=200 after ${i}s db=$DB"; exit 0; }
    sleep 1
  done
  echo "FAILED to become healthy"; tail -20 "$LOG"; exit 1
  ;;
down)
  if [ -f "$PIDF" ]; then kill "$(cat "$PIDF")" 2>/dev/null; sleep 2; rm -f "$PIDF"; fi
  su postgres -c "psql -qc \"DROP DATABASE IF EXISTS $DB WITH (FORCE)\"" >/dev/null 2>&1
  echo "down"
  ;;
*)
  echo "usage: $0 up|down"; exit 2
  ;;
esac
