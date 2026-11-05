#!/usr/bin/env bash
# ============================================================================
# BIZPERM · SOP §9 四步演练（在**隔离副本**上跑，不碰生产）
# ----------------------------------------------------------------------------
# 四步：① 现状对平 → ② 快照+施工 → ③ 幂等重放（0 变更）→ ④ 回滚（回到基线）
#       ⑤ 再施工（证明来回可逆）
#
# 用法：
#   export BIZPERM_DB=<副本库> NIK_DATABASE_HOST/USER/PASSWORD ...
#   bash verify_drill.sh
# 退出码：0 = 全部断言通过
# ============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

: "${NIK_DATABASE_PASSWORD:?需要 NIK_DATABASE_PASSWORD}"
: "${BIZPERM_DB:?需要 BIZPERM_DB（隔离副本库名）}"
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$BIZPERM_DB" -X -q -At)
export FORCE=1

FAIL=0
q() { "${PSQL[@]}" -c "$1"; }
eq() { # eq 名称 期望 实际
    if [ "$2" = "$3" ]; then echo "  [PASS] $1 = $3"; else echo "  [FAIL] $1 期望=$2 实际=$3"; FAIL=1; fi
}

rows_rm()   { q "SELECT count(*) FROM sys_role_menu"; }
rows_menu() { q "SELECT count(*) FROM sys_menu"; }
fm_rows()   { q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id WHERE r.role_code='FINANCE_MANAGER'"; }
base_rm()   { q "SELECT count(*) FROM datafix_bizperm_snapshot WHERE batch='BIZPERM-20261105-ROLEMENU'"; }

echo "== ① 现状（基线）=="
RM0=$(rows_rm); MN0=$(rows_menu); FM0=$(fm_rows)
echo "  role_menu=$RM0 menu=$MN0 finance_manager=$FM0"

echo "== ② 快照 + 施工 =="
bash apply.sh --snapshot >/tmp/bp_step2_snap.log 2>&1 || { echo "  [FAIL] snapshot 失败"; FAIL=1; }
bash apply.sh --grant    >/tmp/bp_step2_grant.log 2>&1 || { echo "  [FAIL] grant 失败"; FAIL=1; }
RM1=$(rows_rm); MN1=$(rows_menu)
# 增量不写死（矩阵会微调）：从施工日志里取「新增授权 N 行 / 新建按钮码 M 条」
ADDED=$(grep -oE "新增授权 [0-9]+ 行" /tmp/bp_step2_grant.log | grep -oE "[0-9]+" | head -1)
NEWMENU=$(grep -oE "新建按钮码 [0-9]+ 条" /tmp/bp_step2_grant.log | grep -oE "[0-9]+" | head -1)
ADDED=${ADDED:-0}; NEWMENU=${NEWMENU:-0}
echo "  日志口径：新增授权 $ADDED 行 / 新建按钮码 $NEWMENU 条"
eq "施工后 role_menu 增量" "$((RM0 + ADDED))" "$RM1"
eq "施工后 menu 增量"      "$((MN0 + NEWMENU))" "$MN1"

echo "== ③ 幂等重放（应 0 变更）=="
bash apply.sh --grant >/tmp/bp_step3.log 2>&1 || { echo "  [FAIL] 重放失败"; FAIL=1; }
grep -q "新增授权 0 行" /tmp/bp_step3.log && echo "  [PASS] 重放 0 行变更" || { echo "  [FAIL] 重放不是 0 变更"; FAIL=1; }
eq "重放后 role_menu 不变" "$RM1" "$(rows_rm)"

echo "== ④ 回滚（应回到基线）=="
bash apply.sh --rollback >/tmp/bp_step4.log 2>&1 || { echo "  [FAIL] 回滚失败"; FAIL=1; }
eq "回滚后 role_menu" "$RM0" "$(rows_rm)"
eq "回滚后 menu"      "$MN0" "$(rows_menu)"
eq "回滚后 FINANCE_MANAGER 授权行" "$FM0" "$(fm_rows)"

echo "== ⑤ 再施工（来回可逆）=="
bash apply.sh --snapshot >/tmp/bp_step5a.log 2>&1 || { echo "  [FAIL] 二次 snapshot 失败"; FAIL=1; }
bash apply.sh --grant    >/tmp/bp_step5b.log 2>&1 || { echo "  [FAIL] 二次 grant 失败"; FAIL=1; }
eq "二次施工后 role_menu" "$RM1" "$(rows_rm)"
eq "二次施工后 menu"      "$MN1" "$(rows_menu)"

echo "== 结果 =="
if [ "$FAIL" = "0" ]; then echo "  四步演练 FAIL=0 ✅"; else echo "  四步演练有 FAIL ❌"; fi
exit "$FAIL"
