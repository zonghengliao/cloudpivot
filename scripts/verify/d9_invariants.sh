#!/usr/bin/env bash
# D9 不变量：干净库 cloudpivot_d9full（全量套件刚跑完）上跑 ERP 不变量审计
set -uo pipefail
DB="${1:-cloudpivot_d9full}"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_DATABASE_NAME="$DB"
python3 /opt/cloudpivot/scripts/verify/erp_invariants.py 2>&1 | tail -25
