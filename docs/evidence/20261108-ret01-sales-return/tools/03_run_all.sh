#!/usr/bin/env bash
# RET-01 一键重放：建库起实例 → 跑完整链路 → 落 raw 与两个落盘日志
#   bash 03_run_all.sh
# 产物：/tmp/ret01_env_out.txt /tmp/ret01_drill_out.txt /tmp/ret01_raw/
set -uo pipefail
D=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
bash "$D/00_env.sh" 2>&1 | tee /tmp/ret01_env_out.txt
echo
bash "$D/01_drill.sh" 2>&1 | tee /tmp/ret01_drill_out.txt
echo
echo "raw 文件数=$(ls /tmp/ret01_raw | wc -l)"
grep -E '^===== 汇总' /tmp/ret01_drill_out.txt
