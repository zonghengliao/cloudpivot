#!/usr/bin/env bash
# D5 单测：真库（cloudpivot_d5，V0137–V0188 全量迁移）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_d5"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
echo "== D5 用例 =="
cargo test --lib business::erp::payable_payment::tests::d5_ -- --test-threads=1 2>&1 | tail -40
echo "== P7/P17 回归（同模块锁，必须一起绿） =="
cargo test --lib business::erp::payable_payment::tests::p7_ -- --test-threads=1 2>&1 | tail -8
cargo test --lib business::erp::payable_bridge::tests::p17_ -- --test-threads=1 2>&1 | tail -8
