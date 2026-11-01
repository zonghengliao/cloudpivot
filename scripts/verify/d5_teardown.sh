#!/usr/bin/env bash
# D5 收尾：停隔离实例 / 停 vite / 删临时验收库（只删本卡造的 cloudpivot_d5full，
# 保留 cloudpivot_d5 供复跑单测）
set -uo pipefail
cd /opt/cloudpivot

echo "== 停隔离实例（按 pid）=="
bash scripts/verify/d5_instance.sh down || true

echo "== 停本卡的 vite（5195）=="
pkill -f "vite --port 5195" 2>/dev/null && echo "vite stopped" || echo "vite 已不在"

sleep 2
echo "== 残留进程检查 =="
pgrep -fa "vite --port 5195" || echo "  无 vite"
pgrep -fa "target/debug/cloudpivot" || echo "  无 debug 实例"

echo "== 删临时全量套件库 cloudpivot_d5full =="
runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS cloudpivot_d5full" && echo "dropped"
runuser -u postgres -- psql -tAc "select datname from pg_database where datname like 'cloudpivot_d5%'"
