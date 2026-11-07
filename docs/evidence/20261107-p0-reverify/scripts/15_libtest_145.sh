#!/usr/bin/env bash
# REVERIFY（t_2f92ee87）回归：145 上按模块分库分进程跑真库 lib 单测（默认 erp）
# 口径沿用 scripts/verify/run_lib_tests_by_module.sh：每模块一个库 + 一个进程，跑完做洁净性校验。
set -uo pipefail
ENVF=/etc/cloudpivot/cloudpivot-dev.env
PW=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' "$ENVF")
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot
export DB_USER=cloudpivot DB_PASS="$PW" MODULES="${MODULES:-erp}" RUNS="${RUNS:-1}"
export TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_libtest_template}"
export LOG_DIR="${LOG_DIR:-/root/reverify/libtest-logs}"
mkdir -p "$LOG_DIR"
echo "== $MODULES RUNS=$RUNS template=$TEMPLATE_DB =="
exec bash scripts/verify/run_lib_tests_by_module.sh
