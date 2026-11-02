#!/usr/bin/env bash
# D1：状态机边配置化的单测（真库 cloudpivot_d1）。
#   $1 = 过滤串（默认 business::shared::state_machine）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend || exit 1

FILTER=${1:-business::shared::state_machine}
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/${D1_DB:-cloudpivot_d1}"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"

cargo test --lib "$FILTER" -- --test-threads=1 --nocapture 2>&1 | tail -60
