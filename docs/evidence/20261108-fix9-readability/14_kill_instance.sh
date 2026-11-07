#!/usr/bin/env bash
# ============================================================================
# FIX-9 145 取证 · 步骤 4：收尾（停隔离实例 + 复核 8033 服役实例健康）
#
# 拷贝库 cloudpivot_fix9_* **保留**（供复核；确认无误后人工 drop）：
#   dropdb -h 127.0.0.1 -U cloudpivot $(cat /root/backup/.fix9_db)
# ============================================================================
set -uo pipefail

if [ -f /root/backup/.fix9_pid ]; then
  kill "$(cat /root/backup/.fix9_pid)" 2>/dev/null || true
  sleep 2
  kill -9 "$(cat /root/backup/.fix9_pid)" 2>/dev/null || true
  rm -f /root/backup/.fix9_pid
fi
pkill -f 'cloudpivot-fix9' 2>/dev/null || true
sleep 1
echo "残留 fix9 进程：$(pgrep -af 'cloudpivot-fix9' | wc -l)"
pgrep -af 'cloudpivot-fix9' || true

echo "8033 服役实例 health=$(curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8033/health)"
echo "8049 隔离实例 health=$(curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8049/health)"
echo "保留的拷贝库：$(cat /root/backup/.fix9_db 2>/dev/null)"
