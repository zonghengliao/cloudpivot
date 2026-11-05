#!/usr/bin/env bash
# 证明「跨模块夹具也会污染菜单审计」：把 menu 审计与 cache_tests / session_tests 一起并行跑
set -u
LABEL="$1"; ROUNDS="$2"
OUT=/root/iso-runs/$LABEL
mkdir -p "$OUT"
export SYS_DB_URL="${SYS_DB_URL:?}"
export ERP_DB_URL="$SYS_DB_URL"; export TASK_DB_URL="$SYS_DB_URL"
cd /opt/cloudpivot/backend || exit 1
: > "$OUT/summary.txt"
for i in $(seq 1 "$ROUNDS"); do
  LOG="$OUT/round-$i.txt"
  cargo test --lib -- system::menu:: system::auth::cache_tests system::session_tests > "$LOG" 2>&1
  echo "round=$i rc=$? $(grep -E '^test result:' "$LOG" | tail -1)" | tee -a "$OUT/summary.txt"
  grep -E '^test .* FAILED' "$LOG" | sed 's/^/  /' >> "$OUT/summary.txt"
  sed -n '/^---- .*n14_menu_auth.*stdout ----/,/^$/p' "$LOG" | head -12 >> "$OUT/summary.txt"
done
cat "$OUT/summary.txt"
