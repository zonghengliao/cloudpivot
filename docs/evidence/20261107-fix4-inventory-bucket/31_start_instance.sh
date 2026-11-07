#!/usr/bin/env bash
# ============================================================================
# FIX-4（t_763677cd）145 取证 · 步骤 2：起隔离实例（全新库，端口 8045）
#
# 前置：30_prepare_145.sh 已跑完（有 /root/backup/.fix4_db_new 与 cloudpivot-fix4 二进制）
# 说明：145 上 8033 的服役实例不动；本实例用完即 kill（33 脚本里会继续用同一个进程跑存量库前先 kill）
# ============================================================================
set -euo pipefail

DB=$(cat /root/backup/.fix4_db_new)
ENVF=/etc/cloudpivot/cloudpivot-dev.env

# 载入 145 的环境（数据库口令 / 管理端口令 / JWT 与存储密钥都在里面）
set -a
# shellcheck disable=SC1090
. "$ENVF"
set +a

export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN=127.0.0.1:8045
export NIK_SEED_DEMO_USERS=true

cd /opt/cloudpivot/backend
setsid nohup ./target/debug/cloudpivot-fix4 > /root/backup/fix4_instance.log 2>&1 < /dev/null &
echo $! > /root/backup/.fix4_pid
echo "pid=$(cat /root/backup/.fix4_pid) db=$DB"

for i in $(seq 1 60); do
  code=$(curl -s -m 2 -o /dev/null -w '%{http_code}' http://127.0.0.1:8045/health || true)
  if [ "$code" = "200" ]; then
    echo "health=200 after ${i}s"
    exit 0
  fi
  sleep 1
done
echo "health 未就绪，日志尾部："
tail -40 /root/backup/fix4_instance.log
exit 1
