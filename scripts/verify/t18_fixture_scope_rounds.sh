#!/usr/bin/env bash
# 卡 t_18dcb777 自证 ①：三个受影响模块在**同一干净库**上、用 **--test-threads=4** 连跑 4 轮
# （第 1 轮 = 干净库首跑；2–4 轮 = 不清库的「残留场景」）。
#
# 注意：system::* 的真库用例只认 SYS_DB_URL（缺它就打印 SKIP 后 return = 假绿），
#       所以这里同时导出 ERP_DB_URL 与 SYS_DB_URL。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
REPO=${REPO:-/opt/cloudpivot}
DB=${ERP_DB:-cloudpivot_18dcb777}
ROUNDS=${ROUNDS:-4}
THREADS=${THREADS:-4}
OUT=${OUT:-/root/18dcb777/module-rounds.log}
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/${DB}"
export SYS_DB_URL="$ERP_DB_URL"

MODS=(
  "business::masking::tests"
  "system::user::tests"
  "system::role::tests"
)

cd "$REPO/backend" || exit 1
: > "$OUT"
fail=0
for r in $(seq 1 "$ROUNDS"); do
  for m in "${MODS[@]}"; do
    log="/root/18dcb777/round-${r}-$(echo "$m" | tr ':' '_').log"
    cargo test --lib "$m" -- --test-threads="$THREADS" > "$log" 2>&1
    line=$(grep -E "^test result:" "$log" | tail -1)
    skips=$(grep -c "^SKIP" "$log")
    echo "round=$r module=$m -> ${line:-<无 test result 行>} (SKIP 行=$skips)" | tee -a "$OUT"
    case "${line:-}" in
      *"0 failed"*) : ;;
      *) fail=1; echo "  !! 非全绿：$log" | tee -a "$OUT"; grep -E "^test .* FAILED" "$log" | tee -a "$OUT" ;;
    esac
    case "${line:-}" in
      *" 0 passed;"*) fail=1; echo "  !! 0 passed：过滤器没打中用例（假绿）" | tee -a "$OUT" ;;
    esac
    if [ "$skips" != "0" ]; then
      fail=1; echo "  !! 出现 SKIP（没连库 = 假绿），见 $log" | tee -a "$OUT"
    fi
  done
done
echo "=== OVERALL: $([ "$fail" -eq 0 ] && echo ALL_GREEN || echo HAS_FAILURE)" | tee -a "$OUT"
exit "$fail"
