#!/usr/bin/env bash
# D2：ERP 不变量审计（条数以脚本汇总行为准 —— 会随新增检查增长）。默认在干净库上跑（干净库才可能全绿）。
#
#   bash scripts/verify/d2_invariants.sh            # 干净库 cloudpivot_d2inv
#   bash scripts/verify/d2_invariants.sh cloudpivot_d2   # 任意库（会因夹具红一片，正常）
#
# ⚠️ 必须导出 NIK_DATABASE_*（erp_invariants.py 只认它们；连不上库会被算成 BLOCKER）。
set -uo pipefail

DB=${1:-${D2_INV_DB:-cloudpivot_d2inv}}
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_DATABASE_NAME="$DB"

echo "== D2 ERP 不变量（库 $DB）=="
python3 "${REPO:-/opt/cloudpivot}/scripts/verify/erp_invariants.py" 2>&1 | tail -20
