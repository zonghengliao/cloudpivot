#!/usr/bin/env bash
# CLS-01 单测（真库）第二部分：只要 period::tests 的用例明细（证明没 SKIP、含新增护栏用例）
set -uo pipefail
DB=cloudpivot_cls01_ut
export PATH=/root/.cargo/bin:$PATH ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/$DB"
cd /opt/cloudpivot/backend || exit 1
cargo test --lib business::erp::period::tests -- --test-threads=1 2>&1 | grep -E "^test business|test result|SKIP"
