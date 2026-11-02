#!/usr/bin/env bash
# D3：跑模块单测（默认 business::flow::linkage，可传过滤串）。
#
#   bash scripts/verify/d3_unit_tests.sh                        # linkage 模块
#   bash scripts/verify/d3_unit_tests.sh business::erp::voucher # 凭证（回归：抽函数后行为不变）
#
# 注意：**必须**带 ERP_DB_URL，否则真库用例会打印 SKIP 假绿。
# 库默认 cloudpivot_d3u（单测专用，别和 e2e 实例的 cloudpivot_d3 共用）。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd "${REPO:-/opt/cloudpivot}/backend" || exit 1

FILTER=${1:-business::flow::linkage}
DB=${D3_DB:-cloudpivot_d3u}
PW=${PGPASSWORD:-alun}
export ERP_DB_URL="${ERP_DB_URL:-postgres://alun:${PW}@127.0.0.1:5432/${DB}}"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"

cargo test --lib "$FILTER" -- --test-threads=1 --nocapture 2>&1 | tail -80
