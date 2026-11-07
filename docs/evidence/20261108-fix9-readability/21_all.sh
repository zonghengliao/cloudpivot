#!/usr/bin/env bash
# ============================================================================
# FIX-9 145 取证 · 一键跑（**长任务**：含 cargo build，必须后台跑）
#
#   ssh root@172.18.10.145 'cd /opt/cloudpivot && \
#     setsid nohup bash docs/evidence/20261108-fix9-readability/21_all.sh \
#       > /root/backup/fix9_all.log 2>&1 < /dev/null &'
#   然后轮询：grep -c 'RESULT:' /root/backup/fix9_all.log
#
# 顺序：拉代码 + 建二进制副本 + 拷贝库 → 起 8049 → 三条验收 → 收尾（8033 全程不动）
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "===== [1/4] prepare ====="
bash "$EV/10_prepare_145.sh" || { echo "prepare FAILED"; exit 1; }

echo "===== [2/4] start isolated instance ====="
bash "$EV/11_start_instance.sh" || { echo "start FAILED"; exit 1; }

echo "===== [3/4] e2e（三条验收）====="
bash "$EV/20_e2e.sh" 2>&1 | tee "$EV/raw/50_e2e_console.txt"
FAILS=$(cat /tmp/_fix9_fails 2>/dev/null || echo 99)

echo "===== [4/4] teardown ====="
bash "$EV/14_kill_instance.sh" | tee "$EV/raw/60_teardown.txt"

echo
echo "RESULT: $FAILS FAILED / 0 == ALL PASS"
exit "$FAILS"
