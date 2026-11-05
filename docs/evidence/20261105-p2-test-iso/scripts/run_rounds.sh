#!/usr/bin/env bash
# 用途：对 system::menu 连跑 N 轮 cargo test --lib（默认并行或串行），逐轮落日志 + 汇总
# 用法：bash run_rounds.sh <label> <rounds> <test-threads|0=default>
set -u
LABEL="$1"; ROUNDS="$2"; THREADS="$3"
REPO=/opt/cloudpivot
OUT=/root/iso-runs/$LABEL
mkdir -p "$OUT"
export SYS_DB_URL="${SYS_DB_URL:?}"
export ERP_DB_URL="$SYS_DB_URL"
export TASK_DB_URL="$SYS_DB_URL"
cd "$REPO/backend" || exit 1
: > "$OUT/summary.txt"
for i in $(seq 1 "$ROUNDS"); do
  LOG="$OUT/round-$i.txt"
  if [ "$THREADS" = "0" ]; then
    cargo test --lib system::menu:: > "$LOG" 2>&1
  else
    cargo test --lib system::menu:: -- --test-threads="$THREADS" > "$LOG" 2>&1
  fi
  rc=$?
  res=$(grep -E '^test result:' "$LOG" | tail -1)
  fails=$(grep -cE '^test .* FAILED' "$LOG")
  echo "round=$i rc=$rc $res failed_lines=$fails" | tee -a "$OUT/summary.txt"
  grep -E '^test .* FAILED' "$LOG" | sed "s/^/  /" >> "$OUT/summary.txt"
done
echo "=== DONE $LABEL ==="
cat "$OUT/summary.txt"
