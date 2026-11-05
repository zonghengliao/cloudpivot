#!/usr/bin/env bash
# F) 白名单机制的端到端验证（在 backend/src 的**临时副本**上做，不碰工作区）
#   F1 只加一条没人承载的注解 → 必须红并点名
#   F2 给它加 `// PERM-EXEMPT: <理由>` → 必须放行，并出现在「白名单放行」明细里
#   F3 把理由写成空（裸白名单）→ 必须重新红，并明确报「没有理由」
set -u
REPO=${REPO:-/opt/cloudpivot}
OUT=${OUT:-$REPO/docs/evidence/20261105-perm-menu-gate}
SNAP=${SNAP:-/root/.hermes/profiles/backend-developer/cache/scratch/prod_menu_codes.txt}
T=$(mktemp -d /tmp/permtree.XXXXXX)
mkdir -p "$T/backend"
cp -a "$REPO/backend/src" "$T/backend/src"
PROBE="$T/backend/src/business/erp/zz_permexempt_probe.rs"
mkdir -p "$OUT"

run() { python3 "$REPO/scripts/verify/perm_menu_gate.py" --repo "$T" --menu-codes-file "$SNAP"; }

{
  echo "# F) 白名单机制端到端（仓库副本：$T）"
  echo
  echo "########## F1 裸注解（菜单里没有这个码）→ 期望 EXIT=1 并点名 ##########"
  printf '#[nik::permission(\n    permission = "probe:not:in:menu",\n)]\nasync fn probe() {}\n' > "$PROBE"
  cat "$PROBE"
  run
  echo "EXIT=$?"
  echo
  echo "########## F2 加 // PERM-EXEMPT: <理由> → 期望 EXIT=0 且进「白名单放行」 ##########"
  printf '// PERM-EXEMPT: 探针：仅用于验证白名单机制，验证完即删（本文件不进仓库）\n#[nik::permission(\n    permission = "probe:not:in:menu",\n)]\nasync fn probe() {}\n' > "$PROBE"
  cat "$PROBE"
  run
  echo "EXIT=$?"
  echo
  echo "########## F3 裸白名单（// PERM-EXEMPT: 后面什么都没有）→ 期望 EXIT=1 ##########"
  printf '// PERM-EXEMPT:\n#[nik::permission(\n    permission = "probe:not:in:menu",\n)]\nasync fn probe() {}\n' > "$PROBE"
  cat "$PROBE"
  run
  echo "EXIT=$?"
} > "$OUT/04_whitelist_e2e.txt" 2>&1

rm -rf "$T"
echo "写出 $OUT/04_whitelist_e2e.txt"
grep -nE "##########|EXIT=|missing_in_menu =|白名单放行|没有理由|理由为空|  · " "$OUT/04_whitelist_e2e.txt"
