#!/usr/bin/env bash
# perm_menu_gate.py 的验收证据（全部原始输出，不转述）
#   A) 隔离库（迁移完整）连库跑 → missing_in_menu = 0
#   B) 软删一条 sys_menu 行（= 注释掉一行）→ 必须 exit 1 并点名该码
#   C) 恢复该行 → 回到 exit 0（证明是那一行导致的红，不是别的原因）
#   D) 生产库快照离线比对（主分支代码 vs 172.18.10.144 的 sys_menu）
#   E) 判据自检 / 白名单裸登记反例
set -u
REPO=${REPO:-/opt/cloudpivot}
OUT=${OUT:-$REPO/docs/evidence/20261105-perm-menu-gate}
DB=${DB:-cloudpivot_permgate}
CODE=${CODE:-business:erp:voucher:audit}     # 就是本卡来源里那 3 条之一
SNAP=${SNAP:-/root/.hermes/profiles/backend-developer/cache/scratch/prod_menu_codes.txt}
mkdir -p "$OUT"

export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_DATABASE_NAME="$DB"

q() { PGPASSWORD=$NIK_DATABASE_PASSWORD psql -h "$NIK_DATABASE_HOST" -U "$NIK_DATABASE_USER" -d "$DB" -Atc "$1"; }

{
  echo "# perm_menu_gate 验收证据 · $(date -Is)"
  echo "# repo=$REPO db=$DB"
  echo "# git HEAD: $(git -C "$REPO" log --oneline -1)"
  echo
  echo "################ A) 连库跑（隔离库，迁移完整） ################"
  echo "\$ NIK_DATABASE_NAME=$DB python3 scripts/verify/perm_menu_gate.py"
  python3 "$REPO/scripts/verify/perm_menu_gate.py"
  echo "EXIT=$?"
  echo
  echo "################ B) 软删一条 sys_menu 行 → 闸必须红 ################"
  echo "\$ UPDATE sys_menu SET deleted_at = now() WHERE auth_code = '$CODE'"
  q "UPDATE sys_menu SET deleted_at = now() WHERE auth_code = '$CODE'" && echo "（已软删）"
  echo "剩余活跃行：$(q "select count(*) from sys_menu where auth_code = '$CODE' and deleted_at is null")"
  echo "\$ python3 scripts/verify/perm_menu_gate.py"
  python3 "$REPO/scripts/verify/perm_menu_gate.py"
  echo "EXIT=$?"
  echo
  echo "################ C) 恢复该行 → 闸回到绿 ################"
  q "UPDATE sys_menu SET deleted_at = NULL WHERE auth_code = '$CODE'" && echo "（已恢复）"
  echo "活跃行：$(q "select count(*) from sys_menu where auth_code = '$CODE' and deleted_at is null")"
  echo "\$ python3 scripts/verify/perm_menu_gate.py"
  python3 "$REPO/scripts/verify/perm_menu_gate.py"
  echo "EXIT=$?"
  echo
  echo "################ D) 生产快照离线比对（主分支代码 vs 生产 sys_menu） ################"
  echo "\$ python3 scripts/verify/perm_menu_gate.py --menu-codes-file <生产快照>"
  head -3 "$SNAP"
  echo "快照码数：$(grep -vc '^#' "$SNAP")"
  python3 "$REPO/scripts/verify/perm_menu_gate.py" --menu-codes-file "$SNAP"
  echo "EXIT=$?"
  echo
  echo "################ E) 判据自检 ################"
  python3 "$REPO/scripts/verify/perm_menu_gate.py" --self-test
  echo "EXIT=$?"
} > "$OUT/01_gate_run.txt" 2>&1

echo "写出：$OUT/01_gate_run.txt"
grep -nE "^(################|EXIT=|✓|✗|ℹ )" "$OUT/01_gate_run.txt" | head -40
