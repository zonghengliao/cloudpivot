#!/usr/bin/env bash
# P1-FIX-2：受影响模块的真库单测（ERP_DB_URL 指向 cloudpivot_p1fix2_b）。
# 只跑受影响的模块（卡面要求：不跑无关的全量回归）。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend || exit 1
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_p1fix2_b"

run() {
  local label="$1" filter="$2" log="$3"
  echo "==================== $label ===================="
  cargo test --lib "$filter" -- --test-threads=1 >"$log" 2>&1
  local code=$?
  tail -4 "$log"
  echo "exit=$code  log=$log"
}

run "fifo round2" "business::erp::stockout_model::fifo_tests" /root/p1fix2_unit_r2.log
run "出库单既有真库用例（DC-13）" "business::erp::stockout_model::tests" /root/p1fix2_stockout_legacy.log
run "库位字典用例（P2-4 出库 FIFO / 指定）" "business::erp::stockin_handler_tests_location_dict" /root/p1fix2_location_dict.log
