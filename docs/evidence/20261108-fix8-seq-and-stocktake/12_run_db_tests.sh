#!/usr/bin/env bash
# ============================================================================
# FIX-8 145 取证 · 步骤 2b：在**拷贝库**上跑本卡触及的真库用例
#
# 跑什么：
#   · business::erp::voucher::tests::fix8 —— 本卡新增的两条取号器用例
#       fix8_concurrent_takes_never_duplicate（10 并发事务不重号）
#       fix8_seq_types_advance_independently（按单据类型独立递增）
#   · business::erp::budget_guard::tests —— 问题 3 的记账口径（recorded_mode 取槽级最严）
#
# 为什么在 145 跑而不是 99：99 本地库是空壳（没有 schema），真库用例只能在有迁移的库上跑。
#
# 跑法（先跑完 11_start_instance.sh）：
#   ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix8-seq-and-stocktake/12_run_db_tests.sh'
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DB=$(cat /root/backup/.fix8_db)
PW=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
export ERP_DB_URL="postgres://cloudpivot:${PW}@127.0.0.1:5432/${DB}"

: > "$EV/raw/db_tests.log"
exec > >(tee -a "$EV/raw/db_tests.log") 2>&1

echo "=== FIX-8 真库用例 $(date '+%F %T') db=$DB ==="
cd /opt/cloudpivot/backend

set +e
cargo test --lib -- \
  business::erp::voucher::tests::fix8 \
  business::erp::budget_guard::tests 2>&1 | tail -40
RC=${PIPESTATUS[0]}
echo "cargo test exit=$RC"
echo "$RC" > /tmp/_fix8_db_tests_rc
