#!/usr/bin/env bash
# D3：ERP 不变量审计（脚本现为 36 条）。必须在**干净库**上跑才可能全绿。
#
#   bash scripts/verify/d3_invariants.sh                # 默认 cloudpivot_d3inv2
#   bash scripts/verify/d3_invariants.sh cloudpivot_d3  # 任意库（有夹具时会红一片，正常）
#
# ⚠️ 必须导出 NIK_DATABASE_*（erp_invariants.py 只认它们；连不上库会被算成 BLOCKER）。
set -uo pipefail

DB=${1:-${D3_INV_DB:-cloudpivot_d3inv2}}
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=${PGPASSWORD:-alun}
export NIK_DATABASE_NAME="$DB"

echo "== D3 ERP 不变量（库 $DB）=="
python3 "${REPO:-/opt/cloudpivot}/scripts/verify/erp_invariants.py" 2>&1 | tail -20
