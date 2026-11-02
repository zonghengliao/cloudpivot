#!/usr/bin/env bash
# D1：全量后端单测（真库 cloudpivot_d1），日志落 /root/d1-full-test.log
# 说明：`cargo test` 默认并行会把 ld 打死（144/99 都踩过 OOM），一律 --test-threads=1。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend || exit 1

export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/${D1_DB:-cloudpivot_d1}"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"

LOG=/root/d1-full-test.log
cargo test --lib -- --test-threads=1 > "$LOG" 2>&1
echo "CARGO_EXIT=$?" >> "$LOG"
