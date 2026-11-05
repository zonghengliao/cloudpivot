#!/usr/bin/env bash
# P1-FIX-2 收尾：clippy（本卡文件零告警）+ fmt 后重跑本卡单测（第三轮，证明最终树也绿）。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend || exit 1
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_p1fix2_b"

echo "==================== clippy --lib ===================="
cargo clippy --lib --all-targets >/root/p1fix2_clippy.log 2>&1
echo "clippy exit=$?"
echo "--- 本卡文件的告警（应为空）---"
grep -n "stockout_model.rs\|stockout_handler.rs\|stockout_dto.rs\|stockout_fifo_tests.rs" /root/p1fix2_clippy.log | head -20
echo "--- 告警总数 ---"
grep -c "^warning" /root/p1fix2_clippy.log

echo "==================== fifo 单测 round3（fmt 后最终树） ===================="
cargo test --lib business::erp::stockout_model::fifo_tests -- --test-threads=1 \
  >/root/p1fix2_unit_r3.log 2>&1
echo "test exit=$?"
tail -4 /root/p1fix2_unit_r3.log
