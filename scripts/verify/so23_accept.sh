#!/usr/bin/env bash
# SO-23 验收：干净库连跑两轮 + 正向/负向演练，日志落 docs/evidence
set -uo pipefail
DIR=/opt/cloudpivot/docs/evidence/20261106-so23-sales-invariants
mkdir -p "$DIR/raw"
export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
       NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun
INV=/opt/cloudpivot/scripts/verify/erp_invariants.py

echo "== 轮 1：干净库 cloudpivot_so23fresh 全量"
NIK_DATABASE_NAME=cloudpivot_so23fresh python3 "$INV" > "$DIR/raw/fresh-round1.log" 2>&1
echo "   exit=$? $(grep -E '^结果' "$DIR/raw/fresh-round1.log")"

echo "== 轮 2：同一库再跑一次（幂等 / 无残留）"
NIK_DATABASE_NAME=cloudpivot_so23fresh python3 "$INV" > "$DIR/raw/fresh-round2.log" 2>&1
echo "   exit=$? $(grep -E '^结果' "$DIR/raw/fresh-round2.log")"

echo "== 差异（两轮结果必须逐字节一致）"
diff -u "$DIR/raw/fresh-round1.log" "$DIR/raw/fresh-round2.log" > "$DIR/raw/round1-vs-round2.diff" \
  && echo "   一致（diff 空）" || echo "   不一致！见 raw/round1-vs-round2.diff"

echo "== 正向夹具库（完整销售链，非空覆盖）"
NIK_DATABASE_NAME=cloudpivot_so23pos python3 "$INV" --group 销售 > "$DIR/raw/pos-sales-group.log" 2>&1
echo "   exit=$? $(grep -E '^结果' "$DIR/raw/pos-sales-group.log")"
NIK_DATABASE_NAME=cloudpivot_so23pos python3 "$INV" > "$DIR/raw/pos-full.log" 2>&1
echo "   full exit=$? $(grep -E '^结果' "$DIR/raw/pos-full.log")"

echo "== 负向演练库（S1..S8 各 1 处违约）"
NIK_DATABASE_NAME=cloudpivot_so23neg python3 "$INV" --group 销售 > "$DIR/raw/neg-sales-group.log" 2>&1
echo "   exit=$? $(grep -E '^结果' "$DIR/raw/neg-sales-group.log")"

echo "== 不进销售组的旧组回归（干净库）"
NIK_DATABASE_NAME=cloudpivot_so23fresh python3 "$INV" --group 会计 > "$DIR/raw/fresh-group-a.log" 2>&1
echo "   会计组 exit=$? $(grep -E '^结果' "$DIR/raw/fresh-group-a.log")"
