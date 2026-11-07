#!/usr/bin/env bash
# ============================================================================
# FIX-5（t_9a23375a）145 取证 · 步骤 2：起隔离实例（8046，连拷贝库）
#
# 前置：10_prepare_145.sh 跑完（有 /root/backup/.fix5_db 与 target/debug/cloudpivot-fix5）
# 迁移在这里发生：日志出现「数据库迁移完成」即 V0325 已入库（存量库升级路径）。
# ============================================================================
set -euo pipefail

DB=$(cat /root/backup/.fix5_db)
ENVF=/etc/cloudpivot/cloudpivot-dev.env

set -a
# shellcheck disable=SC1090
. "$ENVF"
set +a

export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN=127.0.0.1:8046
# 演示账号开关保持 145 的既有配置，不动
cd /opt/cloudpivot/backend
setsid nohup ./target/debug/cloudpivot-fix5 > /root/backup/fix5_instance.log 2>&1 < /dev/null &
echo $! > /root/backup/.fix5_pid
echo "pid=$(cat /root/backup/.fix5_pid) db=$DB port=8046"

for i in $(seq 1 90); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' http://127.0.0.1:8046/health || true)
  if [ "$code" = "200" ]; then
    echo "health=200 after ${i}s"
    grep -E "迁移完成|migration" /root/backup/fix5_instance.log | tail -5
    exit 0
  fi
  sleep 1
done
echo "health 未就绪，日志尾部："
tail -40 /root/backup/fix5_instance.log
exit 1
