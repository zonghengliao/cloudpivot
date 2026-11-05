#!/usr/bin/env bash
# J 组的三个分支各跑一遍，证明这条巡检不是「怎么写都绿」
#
#   A. 老库（V0245 未应用，4 行 status=0）        → J1 必须红（BLOCKER），带定位
#   B. 有 sys_job 但近 7 天没有执行记录的库        → J2 记 INFO 跳过（并显式打印原因）
#   C. 已启用且跑过的库（隔离实例）                → J1/J2 全绿
set -uo pipefail
cd /opt/cloudpivot
run() { # $1=库名  $2=说明
  echo "=================================================================="
  echo "== $2（库 $1）"
  echo "=================================================================="
  NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=alun \
  NIK_DATABASE_NAME="$1" NIK_DATABASE_PASSWORD=alun \
    python3 scripts/verify/erp_invariants.py --group 任务 2>&1 | sed -n '/^J\./,$p'
  echo
}

run cloudpivot_p24   "A. 老库：V0245 未应用 → J1 必须红"
run cloudpivot_fin1  "B. 有 sys_job、无执行记录 → J2 走 INFO 跳过"
run cloudpivot_p0fix1 "C. 修复后的库：J1/J2 全绿"
