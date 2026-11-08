#!/bin/bash
# CODE-09 后端回归（真库，不用 SKIP 假绿）
#
# 本卡**不改后端代码**（CODE-01~08 已交付），所以这里跑的是回归：
#   · 编码号段 / 编码策略（seq、code_policy）—— 编码机制本体
#   · 六类主数据的建单用例（handler / customer_handler / asset_handler / expense_handler）
#   · 全量 --lib（对基线 23 条他卡红做归因，证明本卡路径零红点）
#
# 真库：cloudpivot_code09t（先由隔离实例首启应用 275 个迁移建好 schema）
#
# 用法：bash 03_backend_tests.sh [full]
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export CARGO_TARGET_DIR=/opt/cloudpivot/backend/target
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
LOG="$RAW/03_backend_tests.txt"
: > "$LOG"
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_code09t"
cd /root/cp-code09-wt/backend || exit 1

run() { # $1=标题 $2...=cargo test 过滤参数
  local title=$1; shift
  echo "" | tee -a "$LOG"
  echo "=== $title ===" | tee -a "$LOG"
  cargo test --lib -- "$@" --test-threads=1 2>&1 | tee -a "$LOG" | grep -E "^(test result|running|error|warning: unused)" | tail -4
}

echo "ERP_DB_URL=$ERP_DB_URL" | tee -a "$LOG"
echo "HEAD=$(git -C /root/cp-code09-wt rev-parse --short HEAD)  时间=$(date '+%F %T %Z')" | tee -a "$LOG"

run "编码号段 seq"                       business::erp::seq::
run "编码策略 code_policy"               business::erp::code_policy::
run "供应商/物料/仓库 handler 建单"       business::erp::handler::tests
run "客户 handler 建单"                  business::erp::customer_handler::
run "资产分类 handler 建单"              business::erp::asset_handler::
run "费用类型 handler 建单"              business::erp::expense_handler::

if [ "${1:-}" = "full" ]; then
  echo "" | tee -a "$LOG"
  echo "=== 全量 --lib（归因用）===" | tee -a "$LOG"
  cargo test --lib 2>&1 | tee -a "$LOG" | grep -E "^(test result|failures:)" | tail -5
fi

echo "" | tee -a "$LOG"
{
  echo "汇总：PASS 用例行数 = $(grep -c '^test .* \.\.\. ok$' "$LOG")"
  echo "      FAILED 用例行数 = $(grep -c '^test .* \.\.\. FAILED$' "$LOG")"
  echo "      SKIP/ignored 行数 = $(grep -c 'ignored' "$LOG")"
  grep -E '^test result' "$LOG" | sed 's/^/  /'
} | tee -a "$LOG"
