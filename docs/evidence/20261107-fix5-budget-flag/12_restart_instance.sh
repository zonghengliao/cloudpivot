#!/usr/bin/env bash
# ============================================================================
# FIX-5（t_9a23375a）145 取证 · 步骤 3：重启隔离实例（换了二进制后用）
#
# 只动自己的进程（pid 文件 /root/backup/.fix5_pid），8033 服役实例绝不碰。
# 重启时会把新迁移（V0326）补上：日志出现「应用 1 个迁移」即升级路径成立。
# ============================================================================
set -euo pipefail

DB=$(cat /root/backup/.fix5_db)
ENVF=/etc/cloudpivot/cloudpivot-dev.env

if [ -f /root/backup/.fix5_pid ]; then
  OLD=$(cat /root/backup/.fix5_pid)
  if kill -0 "$OLD" 2>/dev/null; then
    kill "$OLD"
    for _ in $(seq 1 20); do
      kill -0 "$OLD" 2>/dev/null || break
      sleep 0.5
    done
    echo "old instance stopped (pid=$OLD)"
  fi
fi

set -a
# shellcheck disable=SC1090
. "$ENVF"
set +a
export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN=127.0.0.1:8046

cd /opt/cloudpivot/backend
setsid nohup ./target/debug/cloudpivot-fix5 > /root/backup/fix5_instance.log 2>&1 < /dev/null &
echo $! > /root/backup/.fix5_pid
echo "pid=$(cat /root/backup/.fix5_pid) db=$DB port=8046"

for i in $(seq 1 90); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' http://127.0.0.1:8046/health || true)
  if [ "$code" = "200" ]; then
    echo "health=200 after ${i}s"
    grep -E "迁移完成|应用 [0-9]+ 个迁移" /root/backup/fix5_instance.log | tail -3
    exit 0
  fi
  sleep 1
done
echo "health 未就绪，日志尾部："
tail -40 /root/backup/fix5_instance.log
exit 1
