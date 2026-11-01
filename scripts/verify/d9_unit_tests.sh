#!/usr/bin/env bash
# D9 单测：真库 cloudpivot_d9（V0137–V0188 全量迁移）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_d9"
export TASK_DB_URL="$ERP_DB_URL"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"

echo "== D9 用例（收货 / 调拨 / 任务 复制） =="
cargo test --lib d9_ -- --test-threads=1 2>&1 | tail -40
echo "== D9 纯逻辑（副本标题） =="
cargo test --lib business::task::handler::tests::copy_title -- --test-threads=1 2>&1 | tail -12
