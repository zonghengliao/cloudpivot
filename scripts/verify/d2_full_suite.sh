#!/usr/bin/env bash
# D2：全量后端单测（真库 cloudpivot_d2inv），日志落 /root/d2-full-test.log
#
# 说明：`cargo test` 默认并行会把 ld 打死（内存不够），一律 --test-threads=1。
# 用干净库跑（先起一次实例把迁移跑完），避免别的卡的夹具影响全库口径用例。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd "${REPO:-/opt/cloudpivot}/backend" || exit 1

export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/${D2_FULL_DB:-cloudpivot_d2inv}"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"

LOG=${D2_FULL_LOG:-/root/d2-full-test.log}
cargo test --lib -- --test-threads=1 > "$LOG" 2>&1
echo "CARGO_EXIT=$?" >> "$LOG"
grep -E "^test result:|^CARGO_EXIT=" "$LOG" | tail -3
