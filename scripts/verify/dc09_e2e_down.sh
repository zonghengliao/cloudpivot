#!/usr/bin/env bash
# DC-09 真机验收收尾：杀掉 dc09_e2e_up.sh 起的实例（库保留，便于复查数据）
#
#   E2E_DB=cp_units_e2e bash scripts/verify/dc09_e2e_down.sh
set -uo pipefail
DB="${E2E_DB:-cp_units_e2e}"
PIDFILE="/tmp/dc09_e2e_${DB}.pid"
if [ -f "$PIDFILE" ]; then
  PID=$(cat "$PIDFILE")
  kill "$PID" 2>/dev/null && echo "killed pid=$PID"
  sleep 2
  kill -9 "$PID" 2>/dev/null || true
  rm -f "$PIDFILE"
else
  echo "没有 pidfile：$PIDFILE"
fi
echo "db=$DB 保留（要清掉：runuser -u postgres -- psql -qc 'DROP DATABASE IF EXISTS $DB'）"
