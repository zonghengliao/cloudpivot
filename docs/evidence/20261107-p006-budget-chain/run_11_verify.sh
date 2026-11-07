#!/usr/bin/env bash
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/20261107-p006-budget-chain
source ./00_env.sh
OUT="$EV/11_verify_output.txt"
psqlf ./11_verify.sql 2>&1 | tee "$OUT"
echo "--- 输出已落盘：$OUT ---"
