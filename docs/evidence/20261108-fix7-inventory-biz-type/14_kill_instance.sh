#!/usr/bin/env bash
# ============================================================================
# FIX-7 145 取证 · 步骤 4：收尾（停隔离实例 + 复核 8033 服役实例健康）
#
# 拷贝库 cloudpivot_fix7_* **保留**（供复核；确认无误后人工 drop）：
#   dropdb -h 127.0.0.1 -U cloudpivot $(cat /root/backup/.fix7_db)
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix7-inventory-biz-type/14_kill_instance.sh'
# ============================================================================
set -uo pipefail

if [ -f /root/backup/.fix7_pid ]; then
  kill "$(cat /root/backup/.fix7_pid)" 2>/dev/null || true
  sleep 2
  kill -9 "$(cat /root/backup/.fix7_pid)" 2>/dev/null || true
  rm -f /root/backup/.fix7_pid
fi
sleep 1
echo "残留 8048/8049 进程：$(pgrep -af 'cloudpivot-fix7' | wc -l)"
pgrep -af 'cloudpivot-fix7' || true

echo "8033 服役实例 health=$(curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8033/health)"
echo "cloudpivot-test: $(systemctl is-active cloudpivot-test)"
echo "保留的拷贝库：$(cat /root/backup/.fix7_db 2>/dev/null)"
