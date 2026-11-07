#!/usr/bin/env bash
# ============================================================================
# FIX-8 145 取证 · 步骤 2：起隔离实例（8048，连拷贝库）
#
# 前置：10_prepare_145.sh 跑完（有 /root/backup/.fix8_db 与 target/debug/cloudpivot-fix8）
# 起实例前先按进程名收尸：残留进程会制造假 health=200（FIX-7 实测）。
# ============================================================================
set -euo pipefail

DB=$(cat /root/backup/.fix8_db)
ENVF=/etc/cloudpivot/cloudpivot-dev.env
PORT="${PORT:-8048}"

pkill -f 'cloudpivot-fix8' 2>/dev/null || true
sleep 1

set -a
# shellcheck disable=SC1090
. "$ENVF"
set +a

export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
cd /opt/cloudpivot/backend
setsid nohup ./target/debug/cloudpivot-fix8 > /root/backup/fix8_instance.log 2>&1 < /dev/null &
echo $! > /root/backup/.fix8_pid
echo "pid=$(cat /root/backup/.fix8_pid) db=$DB port=$PORT"

for i in $(seq 1 120); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
  if [ "$code" = "200" ]; then
    if grep -q 'Address already in use' /root/backup/fix8_instance.log; then
      echo "health=200 但日志报端口占用（假健康）："
      tail -20 /root/backup/fix8_instance.log
      exit 1
    fi
    echo "health=200 after ${i}s"
    grep -E "数据库迁移完成" /root/backup/fix8_instance.log | tail -3
    exit 0
  fi
  sleep 1
done
echo "health 未就绪，日志尾部："
tail -40 /root/backup/fix8_instance.log
exit 1
