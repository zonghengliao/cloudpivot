#!/usr/bin/env bash
# 隔离树里跑 system::menu 多轮（默认并行），逐轮落日志 + 汇总
# 用法: run_tree_rounds.sh <label> <rounds> <test-threads|0=default> [extra cargo args...]
set -u
LABEL="$1"; ROUNDS="$2"; THREADS="$3"; shift 3
OUT=/root/iso-runs/$LABEL
mkdir -p "$OUT"
export SYS_DB_URL="${SYS_DB_URL:?}"
export ERP_DB_URL="$SYS_DB_URL"; export TASK_DB_URL="$SYS_DB_URL"
export CARGO_TARGET_DIR=/root/menu-iso-target
cd /root/menu-iso-tree/backend || exit 1
: > "$OUT/summary.txt"
for i in $(seq 1 "$ROUNDS"); do
  LOG="$OUT/round-$i.txt"
  if [ "$THREADS" = "0" ]; then
    cargo test --lib system::menu:: "$@" > "$LOG" 2>&1
  else
    cargo test --lib system::menu:: -- --test-threads="$THREADS" "$@" > "$LOG" 2>&1
  fi
  echo "round=$i rc=$? $(grep -E '^test result:' "$LOG" | tail -1)" | tee -a "$OUT/summary.txt"
  grep -E '^test .* FAILED' "$LOG" | sed 's/^/  FAILED: /' >> "$OUT/summary.txt"
done
cat "$OUT/summary.txt"
