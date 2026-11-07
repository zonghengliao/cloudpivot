#!/usr/bin/env bash
# ============================================================================
# FIX-7 145 取证 · 步骤 2b：在**拷贝库**上跑本卡触及模块的真库用例
#
# 为什么在 145 跑而不是 99：145 的拷贝库有真实存量数据（销售出库单 / 无源入库单 /
# 历史流水），本卡的三条真库断言（SALE_OUT / SOURCELESS / 对账）才有意义。
# 99 本地库是空壳，跑什么都只能说「代码没崩」。
#
# 跑法（先跑完 11_start_instance.sh，V0330 已应用到拷贝库）：
#   ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix7-inventory-biz-type/12_run_db_tests.sh'
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DB=$(cat /root/backup/.fix7_db)
PW=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
export ERP_DB_URL="postgres://cloudpivot:${PW}@127.0.0.1:5432/${DB}"

: > "$EV/raw/db_tests.log"
exec > >(tee -a "$EV/raw/db_tests.log") 2>&1

echo "=== FIX-7 真库用例 $(date '+%F %T') db=$DB ==="
cd /opt/cloudpivot/backend

# 只跑本卡触及的模块（全量套件在共享库上会因历史数据左右而红，FIX-6 已建卡处理）
set +e
cargo test --lib -- \
  business::erp::shipment_posting::tests \
  business::erp::stockin_handler::stockin_handler_tests_a4 \
  business::erp::stockin_model::stockin_tests \
  business::erp::stockout_model::stockout_tests \
  business::erp::stockout_base::tests \
  business::erp::stockin_base::tests \
  business::shared::doc_kind 2>&1 | tail -80
RC=${PIPESTATUS[0]}
echo "cargo test exit=$RC"
echo "$RC" > /tmp/_fix7_db_tests_rc
