#!/usr/bin/env bash
# t_c3c2793a：在 145 上跑 K5 改前/改后对比（只改 scripts/verify/erp_invariants.py 一个文件，跑完恢复干净树）
set -uo pipefail
DB="${1:-cloudpivot_rev_demo}"
OUT="${2:-/root/k5}"
cd /opt/cloudpivot || exit 1
mkdir -p "$OUT"
git fetch origin -q
echo "origin/main=$(git rev-parse --short origin/main)  worktree=$(git rev-parse --short HEAD)"

git show c8f82c0:scripts/verify/erp_invariants.py > "$OUT/old.py"
install -m 0644 "$OUT/old.py" scripts/verify/erp_invariants.py || exit 1
echo "== 改前（c8f82c0 版脚本）db=$DB"
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export NIK_DATABASE_NAME="$DB"
python3 scripts/verify/erp_invariants.py > "$OUT/before_${DB}.txt" 2>&1
echo "exit=$?"
grep -E "^结果：" "$OUT/before_${DB}.txt"
grep -E "K5 " "$OUT/before_${DB}.txt" | head -3

git checkout -- scripts/verify/erp_invariants.py
git merge --ff-only origin/main -q || exit 1
echo "== 改后（974a883）HEAD=$(git rev-parse --short HEAD) db=$DB"
export NIK_DATABASE_NAME="$DB"
python3 scripts/verify/erp_invariants.py > "$OUT/after_${DB}.txt" 2>&1
echo "exit=$?"
grep -E "^结果：" "$OUT/after_${DB}.txt"
grep -E "K5 |K5b |覆盖：在役" "$OUT/after_${DB}.txt" | head -8
git status --short | head -5
