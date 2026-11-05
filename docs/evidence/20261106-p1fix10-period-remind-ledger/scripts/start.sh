#!/usr/bin/env bash
# P1-FIX-10 验收：隔离实例启停（结账催办 → 台账）
set -u
DB=cloudpivot_r10
PORT=18210
BIN=/root/r10-bin/cloudpivot
LOG=/root/r10-instance.log

start() {
  mkdir -p /root/r10-bin
  cp -f /opt/cloudpivot/backend/target/debug/cloudpivot "$BIN"
  chmod +x "$BIN"
  pkill -f "$BIN" 2>/dev/null
  sleep 1
  cd /opt/cloudpivot/backend || exit 1
  NIK_PROFILE=test \
  NIK_DATABASE_NAME=$DB \
  NIK_DATABASE_HOST=127.0.0.1 \
  NIK_DATABASE_PORT=5432 \
  NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun \
  NIK_SERVER_LISTEN=127.0.0.1:$PORT \
  NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:?}" \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET='${NIK_MIDDLEWARE_AUTH_JWT_SECRET}' \
  CLOUDPIVOT_STORAGE_MASTER_KEY='${CLOUDPIVOT_STORAGE_MASTER_KEY:?}' \
  nohup "$BIN" >"$LOG" 2>&1 &
  echo "started pid=$!"
}

stop() { pkill -f "$BIN" 2>/dev/null; echo stopped; }

case "${1:-start}" in
  start) start ;;
  stop) stop ;;
  restart) stop; sleep 1; start ;;
esac
