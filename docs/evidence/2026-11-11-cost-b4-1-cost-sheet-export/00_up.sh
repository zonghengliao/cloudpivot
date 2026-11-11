#!/usr/bin/env bash
# COST-B4-1 成本计算单导出的隔离实例（起 / 停）—— 只碰本机测试库，不碰生产
#
#   bash 00_up.sh up     # 建库 costb41_ui → 起 :18051 → 自动迁移（V0370~V0384 全量）
#   bash 00_up.sh down   # 停实例 + dropdb
#
# 与 B3-11 / B2-13 的 00_up.sh 同款口径（同一份二进制 / 同一套环境变量），
# 只换库名与端口，便于并行跑卡。
set -uo pipefail
DB="costb41_ui"
PORT="18051"
BIN="/opt/cloudpivot/backend/target/debug/cloudpivot"
PIDF="/tmp/b41_e2e_${PORT}.pid"
LOG="/tmp/b41_e2e_${PORT}.log"

case "${1:-up}" in
up)
  if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF")" 2>/dev/null; then
    echo "already running pid=$(cat "$PIDF")"; exit 0
  fi
  [ -x "$BIN" ] || { echo "缺少二进制：$BIN（先在 backend/ 跑 cargo build）"; exit 1; }
  su postgres -c "psql -qc \"DROP DATABASE IF EXISTS $DB\"" >/dev/null || exit 1
  su postgres -c "psql -qc \"CREATE DATABASE $DB OWNER alun\"" >/dev/null || exit 1
  cd /opt/cloudpivot/backend || exit 1
  export NIK_PROFILE=test NIK_DATABASE_NAME=$DB NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
         NIK_SERVER_LISTEN=127.0.0.1:$PORT NIK_ADMIN_PASSWORD='Admin@123456' \
         NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 64) \
         CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
         X_CONFIG_SECRET_KEY=0123456789abcdef0123456789abcdef
  setsid "$BIN" > "$LOG" 2>&1 < /dev/null &
  echo $! > "$PIDF"
  for i in $(seq 1 180); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
    [ "$code" = "200" ] && { echo "up pid=$(cat "$PIDF") health=200 after ${i}s db=$DB"; exit 0; }
    sleep 1
  done
  echo "FAILED to become healthy"; tail -20 "$LOG"; exit 1
  ;;
down)
  if [ -f "$PIDF" ]; then kill "$(cat "$PIDF")" 2>/dev/null; sleep 2; rm -f "$PIDF"; fi
  su postgres -c "psql -qc \"DROP DATABASE IF EXISTS $DB\"" >/dev/null 2>&1
  echo "down db=$DB"
  ;;
*)
  echo "用法：bash 00_up.sh [up|down]"; exit 2 ;;
esac
