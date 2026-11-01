#!/usr/bin/env bash
# D9 不变量归因：干净库 cloudpivot_d9inv 上
#   ① 跑「收货 + 任务」的 D9 用例 → 不变量必须仍 32/32（本卡不留残留）
#   ② 跑调拨 D9 用例 → 看残留
#   ③ 只跑**既有**调拨用例（本卡之前就有）→ 残留应与 ② 同形（归因：调拨测试夹具自身基线）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=cloudpivot_d9inv
cd /opt/cloudpivot/backend
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/$DB"
export TASK_DB_URL="$ERP_DB_URL"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"

run_inv() { bash /opt/cloudpivot/scripts/verify/d9_invariants.sh "$DB" | grep -E "^结果：|^  · " | head -8; }

echo "== 基线 =="
run_inv

echo "== ① 收货单 D9 用例（8 条）=="
cargo test --lib business::erp::stockin_handler::tests::d9_ -- --test-threads=1 2>&1 | grep -E "^test result:"
echo "== ① 任务 D9 用例（2 条 + 1 条纯逻辑）=="
cargo test --lib business::task::handler::copy_db_tests::d9_ -- --test-threads=1 2>&1 | grep -E "^test result:"
cargo test --lib business::task::handler::tests::copy_title -- --test-threads=1 2>&1 | grep -E "^test result:"
echo "== ① 跑完不变量（期望 32/32）=="
run_inv

echo "== ② 调拨 D9 用例（2 条）=="
cargo test --lib business::erp::transfer_handler::db_tests::d9_ -- --test-threads=1 2>&1 | grep -E "^test result:"
echo "== ② 跑完不变量 =="
run_inv

echo "== ③ 只跑**既有**调拨用例（与本卡无关，用于归因）=="
cargo test --lib business::erp::transfer_handler::db_tests::create_returns_draft_with_card_no_format -- --test-threads=1 2>&1 | grep -E "^test result:"
echo "== ③ 跑完不变量（应与 ② 同形）=="
run_inv
