#!/usr/bin/env bash
# P2-8 · 口径复算连跑两轮（只读）。
#
# 验收要求「连跑两轮」：本脚本把 sql_recompute.sql 跑两次，
#   run-A.sqlrecompute.txt / run-B.sqlrecompute.txt
# 然后 diff —— **必须为空**（SQL 里没有任何 now()/随机/UUID 生成，输出天然确定）。
# 同时把 api_probe.py（只读接口探针）各跑一次，两次输出也应逐字一致
# （登录 401 是稳态：本机 dev 库 admin 口令非仓库默认值，脚本按「不猜凭据」直接 SKIP）。
#
# 用法：bash docs/evidence/20261104-p2-8/recompute_two_rounds.sh
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)   # .../docs/evidence/20261104-p2-8
ROOT=$(cd "$HERE/../.." && pwd)       # .../docs/evidence → 再上一层才是仓库根
[ -f "$ROOT/backend/Cargo.toml" ] || ROOT=$(cd "$HERE/../../.." && pwd)
cd "$ROOT" || exit 1
OUT="$HERE"
[ -f "$ROOT/backend/Cargo.toml" ] || { echo "FATAL: 找不到仓库根（当前 $ROOT）"; exit 1; }

for tag in A B; do
  {
    printf '# round %s @ %s\n' "$tag" "$(date '+%F %T %Z')"
    su postgres -c "psql -q -tA -F'|' -f $OUT/sql_recompute.sql" 2>&1
  } > "$OUT/run-$tag.sqlrecompute.txt"
  python3 "$OUT/api_probe.py" > "$OUT/run-$tag.api_probe.txt" 2>&1
done

fail=0
echo "=== diff（首行时间戳除外，必须为空）==="
for kind in api_probe sqlrecompute; do
  a="$OUT/run-A.$kind.txt"; b="$OUT/run-B.$kind.txt"
  if [ ! -s "$a" ] || [ ! -s "$b" ]; then
    echo "$kind: MISSING/EMPTY 输出（a=$(wc -c <"$a" 2>/dev/null || echo 0)B b=$(wc -c <"$b" 2>/dev/null || echo 0)B）"; fail=1; continue
  fi
  if diff <(tail -n +2 "$a") <(tail -n +2 "$b"); then
    echo "$kind: IDENTICAL ($(wc -l < "$a") 行)"
  else
    echo "$kind: DIFFERS"; fail=1
  fi
done
echo "---"
[ "$fail" = 0 ] && echo "RESULT: 两轮逐字一致" || echo "RESULT: 有两轮不一致项（见上）"
exit "$fail"
