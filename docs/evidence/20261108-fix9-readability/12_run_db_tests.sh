#!/usr/bin/env bash
# ============================================================================
# FIX-9 145 取证 · 步骤 3：在 145 的**拷贝库**上跑本卡新增的 3 条真库用例
#
# 为什么要真库：这三条断言的对象都是数据库事实（result_comment / 凭证摘要 / operator_name），
# `ERP_DB_URL` 不设时用例会打印 SKIP 直接返回 —— 那就是假绿。
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix9-readability/12_run_db_tests.sh'
#   （cargo test 首次要编译 test profile，建议 setsid 后台跑）
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=$(cat /root/backup/.fix9_db)
PW=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
export ERP_DB_URL="postgres://cloudpivot:${PW}@127.0.0.1:5432/${DB}"
export TASK_DB_URL="$ERP_DB_URL"
export P0FIX_DB_URL="$ERP_DB_URL"

cd /opt/cloudpivot/backend
echo "DB=$DB"
cargo test --lib --no-run 2>&1 | tail -2

for name in \
  fix9_voucher_summary_uses_material_code_without_inventory_account \
  operator_name_snapshot_filled_when_only_id_given \
  approval_comment_written_back_to_instance_result_comment ; do
  echo
  echo "===== $name ====="
  cargo test --lib "$name" -- --test-threads=1 --nocapture 2>&1 | grep -E "^test |^test result|SKIP"
done
