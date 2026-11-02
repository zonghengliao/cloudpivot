#!/usr/bin/env bash
# D2：跨单据联动白名单的单测（真库 cloudpivot_d2）。
#   $1 = 过滤串（默认 business::flow::linkage）
#
# 注意：**必须**带 ERP_DB_URL，否则真库用例会打印 SKIP 假绿。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd "${REPO:-/opt/cloudpivot}/backend" || exit 1

FILTER=${1:-business::flow::linkage}
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/${D2_DB:-cloudpivot_d2}"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"

cargo test --lib "$FILTER" -- --test-threads=1 --nocapture 2>&1 | tail -80
