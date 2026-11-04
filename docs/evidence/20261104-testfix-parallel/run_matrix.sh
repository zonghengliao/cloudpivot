#!/usr/bin/env bash
# ============================================================================
# 25926f63_run.sh —— 受影响 4 模块「并发自检」：--test-threads=4 × 3 轮 + 残留重跑
#
# 设计（对齐卡面验收）：
#   PHASE A（同一库、A 轮）：每个模块**连跑 3 次**（module-major），库不清理。
#   PHASE B（残留场景）：先对 4 个模块各跑 1 次（完整一遍，故意留下夹具残留），
#                        紧接着每个模块**再连跑 3 次**（R1..R3），仍须 0 failed。
#   —— 全程同一个隔离库、全程不清库，用来证明「隔离」而不是「清空库」。
#
# 用法：ERP_DB_URL=postgres://... bash /root/25926f63_run.sh
# 输出：/root/25926f63-runs/*.log  +  /root/25926f63-runs/summary.tsv
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
TREE=${TREE:-/root/25926f63-tree}
OUT=${OUT:-/root/25926f63-runs}
THREADS=${THREADS:-4}
MODS=(
  "business::erp::period_items"
  "business::erp::contract_approval_tests"
  "business::erp::payable_bridge"
  "business::erp::stockin_handler"
)

if [ -z "${ERP_DB_URL:-}" ]; then echo "必须设 ERP_DB_URL" >&2; exit 1; fi
mkdir -p "$OUT"
SUM="$OUT/summary.tsv"
: > "$SUM"
cd "$TREE/backend" || exit 1

run_one() {  # $1=phase $2=tag $3=module
  local phase=$1 tag=$2 mod=$3
  local name; name=$(echo "$mod" | sed 's/business::erp:://')
  local log="$OUT/${name}__${phase}__${tag}.log"
  local t0=$SECONDS
  cargo test --lib "$mod" -- --test-threads="$THREADS" > "$log" 2>&1
  local rc=$?
  local dt=$((SECONDS-t0))
  local line; line=$(grep -E "^test result:" "$log" | tail -1)
  local failed; failed=$(echo "${line:-}" | grep -oE "[0-9]+ failed" | grep -oE "^[0-9]+" | head -1)
  local passed; passed=$(echo "${line:-}" | grep -oE "[0-9]+ passed" | grep -oE "^[0-9]+" | head -1)
  local verdict=RED
  if [ "$rc" -eq 0 ] && [ "${failed:-9}" = "0" ] && [ "${passed:-0}" != "0" ]; then verdict=GREEN; fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$phase" "$tag" "$mod" "${passed:-?}" "${failed:-?}" "$verdict" "${dt}s" "$log" >> "$SUM"
  echo "[$phase/$tag] $mod -> ${line:-<no result line>} (${dt}s) => $verdict"
  if [ "$verdict" != GREEN ]; then
    echo "  !! rc=$rc 原始输出：$log"
    grep -E "^test .* FAILED|panicked at" "$log" | head -20
  fi
}

for m in "${MODS[@]}"; do
  for r in 1 2 3; do run_one A "R$r" "$m"; done
done

echo "=== PHASE A DONE, 库内残留状态（进 PHASE B 前） ==="
python3 /root/25926f63_residue.py 2>/dev/null | tee -a "$OUT/residue.txt" || true

for m in "${MODS[@]}"; do run_one B "SEED" "$m"; done
echo "=== PHASE B SEED PASS DONE（故意留下残留） ==="
python3 /root/25926f63_residue.py 2>/dev/null | tee -a "$OUT/residue.txt" || true

for m in "${MODS[@]}"; do
  for r in 1 2 3; do run_one B "R$r" "$m"; done
done

echo "=== PHASE B DONE, 库内残留状态（终态） ==="
python3 /root/25926f63_residue.py 2>/dev/null | tee -a "$OUT/residue.txt" || true

echo "=== SUMMARY ==="
cat "$SUM"
echo "=== VERDICT ==="
if grep -q "RED" "$SUM"; then echo "HAS_FAILURE"; grep "RED" "$SUM"; exit 1; else echo "ALL_GREEN"; exit 0; fi
