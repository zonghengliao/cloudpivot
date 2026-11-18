#!/usr/bin/env bash
# M5-06 反证：把 cost_object_model.rs 的「未归集领料」判据从
# 「按出库明细行反查单头」改回「cc.source_id = 出库单头 id」，跑一遍新用例，
# 期望红；再逐字节还原（sha256 校验），跑一遍期望绿。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
EV=/opt/cloudpivot/docs/evidence/2026-11-19-t_d77cbe01-batch-method-tests
SRC=/opt/cloudpivot/backend/app/src/business/erp/cost/cost_object_model.rs
SCRATCH=/root/.hermes/profiles/backend-developer/cache/scratch
export ERP_DB_URL="postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@172.18.10.145:5432/cp_m506_fresh"
cd /opt/cloudpivot/backend || exit 1

cp "$SRC" "$SCRATCH/m506_model.before_teeth.rs"
BEFORE=$(sha256sum "$SRC" | cut -d' ' -f1)
echo "before_sha256=$BEFORE"

python3 /root/.hermes/profiles/backend-developer/cache/scratch/m506_mutate.py "${1:-mutate}" "$SRC" || exit 1

echo "== 变异后 sha256 =="
sha256sum "$SRC"

echo "== 变异后跑新用例（期望 FAILED）=="
cargo test --lib business::erp::cost::cost_batch_method_tests -- --test-threads=1 > "$EV/04_teeth_header_id_mutation.out" 2>&1
echo "mutated_test_rc=$?"
grep -E "^test business|test result" "$EV/04_teeth_header_id_mutation.out"

echo "== 逐字节还原 =="
cp "$SCRATCH/m506_model.before_teeth.rs" "$SRC"
AFTER=$(sha256sum "$SRC" | cut -d' ' -f1)
echo "after_sha256=$AFTER"
if [ "$BEFORE" != "$AFTER" ]; then
  echo "RESTORE_FAILED"
  exit 1
fi
echo "RESTORE_OK"

echo "== 还原后跑新用例（期望 ok）=="
cargo test --lib business::erp::cost::cost_batch_method_tests -- --test-threads=1 > "$EV/05_restored_green.out" 2>&1
echo "restored_test_rc=$?"
grep -E "test result" "$EV/05_restored_green.out" | head -1
