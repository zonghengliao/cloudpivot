#!/usr/bin/env bash
# REVERIFY 145：起隔离实例（8050，连拷贝库）
set -euo pipefail
DB=$(cat /root/backup/.rev_db)
ENVF=/etc/cloudpivot/cloudpivot-dev.env
PORT="${PORT:-8050}"
pkill -f 'cloudpivot-rev' 2>/dev/null || true
sleep 1
set -a; . "$ENVF"; set +a
export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
cd /opt/cloudpivot/backend
setsid nohup ./target/debug/cloudpivot-rev > /root/backup/rev_instance.log 2>&1 < /dev/null &
echo $! > /root/backup/.rev_pid
echo "pid=$(cat /root/backup/.rev_pid) db=$DB port=$PORT"
for i in $(seq 1 120); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" || true)
  if [ "$code" = "200" ]; then
    echo "health=200 after ${i}s"
    grep -E "数据库迁移完成" /root/backup/rev_instance.log | tail -3
    exit 0
  fi
  sleep 1
done
echo "health 未就绪："; tail -40 /root/backup/rev_instance.log; exit 1
