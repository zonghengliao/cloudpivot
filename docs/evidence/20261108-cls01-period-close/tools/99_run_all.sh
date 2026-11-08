#!/usr/bin/env bash
# CLS-01 一键跑完：建环境 → 1-11 月业务+月结 → 结账验收 → 年结+次年开账
#   bash 99_run_all.sh
set -uo pipefail
cd /tmp/cls01 || exit 1
export CLS01_RAW=/tmp/cls01_raw
mkdir -p "$CLS01_RAW"
OUT=/tmp/cls01/all.out
: >"$OUT"

bash 00_env.sh          >>"$OUT" 2>&1; echo "[00_env exit=$?]" >>"$OUT"
bash 01_monthly.sh      >>"$OUT" 2>&1; echo "[01_monthly exit=$?]" >>"$OUT"
bash 02_close_checks.sh >>"$OUT" 2>&1; echo "[02_close exit=$?]" >>"$OUT"
bash 03_year_end.sh     >>"$OUT" 2>&1; echo "[03_year_end exit=$?]" >>"$OUT"

grep -E "小计|\[0[0-9]_" "$OUT"
