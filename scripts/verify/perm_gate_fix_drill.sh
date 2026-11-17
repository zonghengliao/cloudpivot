#!/usr/bin/env bash
# ============================================================================
# perm_gate_fix_drill.sh —— FIX-权限闸（V0442）真库演练
#
# 卡：t_fa41e0e1（FIX-权限闸：M1-23 账龄页面码死码 + 4 对孤儿按钮）
#
# 做四件事（全部对**临时演练库**，跑完自己 DROP；库名含 drill，只建在 145）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库，补齐模板落后的业务迁移 V0405…最新
#      （模板停在 V0404，perm_menu / orphan 两道闸都要看最新菜单集，否则测的是旧形态）；
#   2. 【基线】在**还没应用 V0442** 的库上跑闸，把「修之前」的红记下来：
#        · perm_menu_gate.py      → missing_in_menu = 0（M2-07 的台账已修好 N14，这里只做回归）
#        · orphan_button_gate.py  → **必须数出 4 对孤儿**（证明闸是活的、也证明缺陷真实存在）
#        · permission_audit_tests → **N20 必须红**（报价/返利 6 个核心码零授予）
#        · menu_invariants.sh     → 8 条结构不变量
#   3. 应用 V0442，逐条验硬指标：
#        · orphan_button_gate  orphan = 0
#        · permission_audit_tests 8/8（真库，SKIP = 0）
#        · menu_invariants.sh 8/8（菜单总数不许涨：本迁移只改授权与 parent_id）
#        · 定向断言：…414 归位到「消息中心」/ FINANCE_DIRECTOR 拿到应收台账页 /
#          TEST_UADMIN 拿到角色互斥页 + 系统管理目录 / 六个 N20 码各有非超管持有者 /
#          SALES_CLERK 无 flow:approve（职责分离）
#   4. 幂等（up 重放 0 行）+ 回滚（down → 计数回到基线 → down 重放 0 → up 恢复）
#
# 用法：
#   bash scripts/verify/perm_gate_fix_drill.sh                 # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/perm_gate_fix_drill.sh   # 在 145 上跑
#   KEEP_DB=1 …                                                # 留库排查（打印库名）
#
# 退出码：0 = 全绿；1 = 任一断言失败。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
APP_DIR="$ROOT/backend/app"
MIG_UP="$MIG_DIR/V0442__perm_gate_orphans_and_n20.up.sql"
MIG_DOWN="$MIG_DIR/V0442__perm_gate_orphans_and_n20.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_permfix_drill_$$"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/tmp}"
LOG="$TMP/permfix_drill_$$.log"
PASS=0
FAIL=0

ok()  { PASS=$((PASS+1)); echo "  ✔ $*"; }
bad() { FAIL=$((FAIL+1)); echo "  ✘ $*"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（= $1）"; else bad "$3：期望 [$2]，实得 [$1]"; fi; }
assert_ge() { if [ "${1:-0}" -ge "${2:-0}" ] 2>/dev/null; then ok "$3（= $1）"; else bad "$3：期望 >= $2，实得 [$1]"; fi; }

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -q -f "$1" >"$LOG" 2>&1; }

cleanup() {
    if [ "$KEEP_DB" = "1" ]; then
        echo "（KEEP_DB=1：演练库保留 $DB）"
    else
        psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    fi
    rm -f "$LOG"
}
trap cleanup EXIT

echo "=============================================================="
echo "FIX-权限闸 V0442 真库演练 · $DB → $PG_HOST"
echo "=============================================================="

# ---------- 0) 克隆模板库 ----------
if ! psql_admin "SELECT 1 FROM pg_database WHERE datname = '$TEMPLATE_DB'" | grep -q 1; then
    echo "✘ 模板库 $TEMPLATE_DB 不在 $PG_HOST（演练库只建在 145）"; exit 1
fi
psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
if psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE_DB" >/dev/null 2>&1; then
    ok "克隆演练库：$TEMPLATE_DB → $DB"
else
    bad "克隆演练库失败（模板库有活动连接？）"; exit 1
fi

# ---------- 1) 补齐模板落后的业务迁移（V0405…最新，不含 V0442） ----------
echo "== 1) 补齐业务迁移 V0405…最新（幂等，逐条 ON_ERROR_STOP）"
psql_q "select name from schema_migrations where kind='versioned'" > "$TMP/_applied_$$.txt"
APPLIED=0
for f in $(ls "$MIG_DIR"/V*.up.sql | sort); do
    base=$(basename "$f"); migname=${base#*__}; migname=${migname%.up.sql}
    vnum=$(echo "$base" | grep -oE '^V[0-9]{4}' | tr -d 'V')
    [ "${vnum:-0}" -lt 405 ] && continue
    [ "$migname" = "perm_gate_orphans_and_n20" ] && continue   # 本卡的迁移留到第 3 步
    grep -qx "$migname" "$TMP/_applied_$$.txt" && continue
    if ! psql_file "$f"; then
        bad "迁移 $base 应用失败（tail: $(tail -3 "$LOG" | tr '\n' ' ')）"; exit 1
    fi
    APPLIED=$((APPLIED+1))
done
rm -f "$TMP/_applied_$$.txt"
ok "补齐 $APPLIED 条业务迁移"
assert_eq "540" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")" "活跃菜单行数（menu_invariants ⑤ 上界 540，本卡不新增菜单）"

# ---------- 2) 基线：V0442 应用前 ----------
echo "== 2) 基线（还没应用 V0442）：把「修之前」的红记下来"
export NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT="$PG_PORT" \
       NIK_DATABASE_USER="$PG_USER" NIK_DATABASE_NAME="$DB" \
       NIK_DATABASE_PASSWORD="$PGPASSWORD"
export SYS_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export ERP_DB_URL="$SYS_DB_URL"

python3 "$ROOT/scripts/verify/perm_menu_gate.py" > "$TMP/_pm_base_$$.out" 2>&1
rc=$?
if [ $rc -eq 0 ]; then ok "perm_menu_gate 基线 PASS（missing_in_menu = 0）"
else bad "perm_menu_gate 基线不是 PASS（rc=$rc，见 tail: $(tail -3 "$TMP/_pm_base_$$.out" | tr '\n' ' ')）"; fi

python3 "$ROOT/scripts/verify/orphan_button_gate.py" > "$TMP/_ob_base_$$.out" 2>&1
BASE_ORPHAN=$(grep -oE '孤儿 \(角色,码\) = [0-9]+' "$TMP/_ob_base_$$.out" | grep -oE '[0-9]+$' | head -1)
assert_eq "${BASE_ORPHAN:-x}" "4" "基线孤儿对数（4 对：notice:mine×2 / receivable:aging×1 / role-mutex:conflict×1）"

(cd "$ROOT/backend" && timeout 1500 cargo test --lib system::menu::permission_audit_tests -- --test-threads=1) \
    > "$TMP/_pa_base_$$.out" 2>&1
BASE_PA="$?"
BASE_SKIP=$(grep -cE '^SKIP' "$TMP/_pa_base_$$.out")
if [ "$BASE_PA" -ne 0 ]; then ok "permission_audit_tests 基线 FAILED（N20 红，符合预期）"
else bad "permission_audit_tests 基线竟然全绿 —— 缺陷没复现，本演练失真"; fi
assert_eq "$BASE_SKIP" "0" "基线 permission_audit_tests 的 SKIP 行数（0 = 真连库，不是假绿）"
grep -q "n20_erp_core_permission_codes_are_granted_to_non_super_role" "$TMP/_pa_base_$$.out" && \
    grep -q "n20_erp_core_permission_codes_are_granted_to_non_super_role ... FAILED" "$TMP/_pa_base_$$.out" && \
    ok "基线失败用例正是 N20（其余 7 条通过）" || bad "基线失败用例不是 N20，需人工看"

bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" > "$TMP/_mi_base_$$.out" 2>&1
grep -q "结果: 全绿" "$TMP/_mi_base_$$.out" && ok "menu_invariants 基线 8 条全绿" || bad "menu_invariants 基线有红"

# ---------- 3) 应用 V0442 ----------
echo "== 3) 应用 V0442（up 单事务），逐条验硬指标"
if psql_file "$MIG_UP"; then ok "V0442 应用成功（自检 NOTICE: $(grep -o 'V0442 自检通过.*' "$LOG" | head -1 | cut -c1-60)…）"
else bad "V0442 应用失败（tail: $(tail -3 "$LOG" | tr '\n' ' ')）"; fi

assert_eq "23000000-0000-0000-0000-000000000010" "$(psql_q "SELECT parent_id FROM sys_menu WHERE id='20000000-0000-0000-0000-000000000414'::uuid")" \
    "business:notice:mine 按钮 …414 归位到「消息中心」页"
assert_eq "0" "$(psql_q "SELECT count(*) FROM (SELECT sort_order FROM sys_menu WHERE parent_id='23000000-0000-0000-0000-000000000010'::uuid AND menu_type=3 AND deleted_at IS NULL GROUP BY 1 HAVING count(*)>1) d")" \
    "「消息中心」下按钮 sort_order 不重号（约定 12）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id WHERE r.role_code='FINANCE_DIRECTOR' AND rm.menu_id='20000000-0000-0000-0000-000000002880'::uuid")" \
    "FINANCE_DIRECTOR 拿到「应收台账」页（business:erp:receivable:list）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id WHERE r.role_code='TEST_UADMIN' AND rm.menu_id='20000000-0000-0000-0000-000000000620'::uuid")" \
    "TEST_UADMIN 拿到「角色互斥」页（system:role-mutex:list）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id WHERE r.role_code='TEST_UADMIN' AND rm.menu_id='10000000-0000-0000-0000-000000000001'::uuid")" \
    "TEST_UADMIN 补到祖先目录「系统管理」（否则页面在侧栏被静默丢弃）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role r JOIN sys_role_menu rm ON rm.role_id=r.id JOIN sys_menu m ON m.id=rm.menu_id WHERE r.role_code='SALES_CLERK' AND m.auth_code='business:flow:approve'")" \
    "职责分离：SALES_CLERK 不持有 business:flow:approve（业务员不能自审报价）"

for code in business:erp:quotation:list business:erp:quotation:create business:erp:quotation:status \
            business:erp:rebate-policy:list business:erp:rebate-policy:create business:erp:rebate-settlement:list; do
    BASE_ROLES=$(psql_q "SELECT count(DISTINCT r.id) FROM sys_menu m JOIN sys_role_menu rm ON rm.menu_id=m.id JOIN sys_role r ON r.id=rm.role_id AND r.deleted_at IS NULL WHERE m.auth_code='$code' AND m.deleted_at IS NULL AND r.role_code <> 'SUPER_ADMIN'")
    assert_ge "${BASE_ROLES:-0}" "1" "N20 码 $code 有非超管持有者"
done

python3 "$ROOT/scripts/verify/orphan_button_gate.py" > "$TMP/_ob_after_$$.out" 2>&1
rc=$?
AFTER_ORPHAN=$(grep -oE '孤儿 \(角色,码\) = [0-9]+' "$TMP/_ob_after_$$.out" | grep -oE '[0-9]+$' | head -1)
assert_eq "${AFTER_ORPHAN:-x}" "0" "orphan_button_gate 孤儿对数"
[ $rc -eq 0 ] && ok "orphan_button_gate 退出码 0" || bad "orphan_button_gate 退出码 $rc"

python3 "$ROOT/scripts/verify/perm_menu_gate.py" > "$TMP/_pm_after_$$.out" 2>&1
rc=$?
[ $rc -eq 0 ] && ok "perm_menu_gate 退出码 0（missing_in_menu = 0）" || bad "perm_menu_gate 退出码 $rc"

(cd "$ROOT/backend" && timeout 1500 cargo test --lib system::menu::permission_audit_tests -- --test-threads=1) \
    > "$TMP/_pa_after_$$.out" 2>&1
PA="$?"
PA_SKIP=$(grep -cE '^SKIP' "$TMP/_pa_after_$$.out")
assert_eq "$PA" "0" "permission_audit_tests 退出码（0 = 8/8）"
assert_eq "$(grep -oE '[0-9]+ passed' "$TMP/_pa_after_$$.out" | head -1)" "8 passed" "permission_audit_tests 通过条数"
assert_eq "$PA_SKIP" "0" "permission_audit_tests 的 SKIP 行数（0 = 真连库）"

bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" > "$TMP/_mi_after_$$.out" 2>&1
grep -q "结果: 全绿" "$TMP/_mi_after_$$.out" && ok "menu_invariants 8 条全绿（菜单总数未涨）" || bad "menu_invariants 有红"

# ---------- 4) 幂等 + 回滚往返 ----------
echo "== 4) 幂等（up 重放）+ 回滚（down → down 重放 → up 恢复）"
GRANT_ROWS() { psql_q "SELECT count(*) FROM sys_role_menu WHERE id IN (
        SELECT md5('v0442.'||r.id||':'||m.id)::uuid
          FROM sys_role r CROSS JOIN sys_menu m
         WHERE m.deleted_at IS NULL
           AND m.auth_code IN ('business:erp:receivable:list','system:role-mutex:list',
                               'business:erp:quotation:list','business:erp:quotation:create',
                               'business:erp:quotation:status','business:erp:rebate-policy:list',
                               'business:erp:rebate-policy:create','business:erp:rebate-settlement:list'))"; }
CLOSURE_ROWS() { psql_q "SELECT count(*) FROM sys_role_menu WHERE id IN (
        SELECT md5('v0442c.'||r.id||':'||d.dir_id)::uuid
          FROM sys_role r
         CROSS JOIN (VALUES ('10000000-0000-0000-0000-000000000001'::uuid),
                            ('20000000-0000-0000-0000-000000002701'::uuid)) d(dir_id))"; }
N_GRANT_BEFORE=$(GRANT_ROWS)
N_CLOSURE_BEFORE=$(CLOSURE_ROWS)
TOTAL_RM_BEFORE=$(psql_q "SELECT count(*) FROM sys_role_menu")
assert_ge "$N_GRANT_BEFORE" "16" "本迁移的授权行数（N20 矩阵 14 + 账龄/互斥两对补授 ≥2）"
assert_ge "${N_CLOSURE_BEFORE:-0}" "2" "本迁移的祖先闭包行数（≥2：销售管理 / 系统管理）"
if psql_file "$MIG_UP"; then ok "V0442 重放成功（幂等）"; else bad "V0442 重放失败"; fi
assert_eq "$(GRANT_ROWS)" "$N_GRANT_BEFORE" "重放后本迁移授权行数不变"
assert_eq "$(CLOSURE_ROWS)" "$N_CLOSURE_BEFORE" "重放后祖先闭包行数不变"
assert_eq "$(psql_q "SELECT count(*) FROM sys_role_menu")" "$TOTAL_RM_BEFORE" "重放后 sys_role_menu 总行数不变"

if psql_file "$MIG_DOWN"; then ok "V0442 down 成功"; else bad "V0442 down 失败（tail: $(tail -3 "$LOG" | tr '\n' ' ')）"; fi
assert_eq "20000000-0000-0000-0000-000000000410" "$(psql_q "SELECT parent_id FROM sys_menu WHERE id='20000000-0000-0000-0000-000000000414'::uuid")" \
    "down 后 …414 回到「消息通知」页"
assert_eq "40" "$(psql_q "SELECT sort_order FROM sys_menu WHERE id='20000000-0000-0000-0000-000000000414'::uuid")" \
    "down 后 …414 的 sort_order 回到 40"
assert_eq "$(GRANT_ROWS)" "0" "down 后本迁移授权行清零"
assert_eq "$(CLOSURE_ROWS)" "0" "down 后祖先闭包行清零"
assert_eq "$(psql_q "SELECT count(*) FROM sys_role_menu")" "$((TOTAL_RM_BEFORE - N_GRANT_BEFORE - N_CLOSURE_BEFORE))" \
    "down 后 sys_role_menu 回到基线（授权 $N_GRANT_BEFORE + 目录 $N_CLOSURE_BEFORE 全清）"
python3 "$ROOT/scripts/verify/orphan_button_gate.py" > "$TMP/_ob_down_$$.out" 2>&1
DOWN_ORPHAN=$(grep -oE '孤儿 \(角色,码\) = [0-9]+' "$TMP/_ob_down_$$.out" | grep -oE '[0-9]+$' | head -1)
assert_eq "${DOWN_ORPHAN:-x}" "4" "down 后孤儿对数回到基线 4（闸因此证明自己不是永远绿）"

if psql_file "$MIG_DOWN"; then ok "down 可重放（幂等）"; else bad "down 重放失败"; fi
assert_eq "$(psql_q "SELECT count(*) FROM sys_role_menu")" "$((TOTAL_RM_BEFORE - N_GRANT_BEFORE - N_CLOSURE_BEFORE))" \
    "down 重放后 sys_role_menu 行数不变"

if psql_file "$MIG_UP"; then ok "down 之后再 up：恢复成功"; else bad "down 之后 up 失败"; fi
assert_eq "$(GRANT_ROWS)" "$N_GRANT_BEFORE" "恢复后授权行数与首次应用一致"
python3 "$ROOT/scripts/verify/orphan_button_gate.py" > "$TMP/_ob_restore_$$.out" 2>&1
RESTORE_ORPHAN=$(grep -oE '孤儿 \(角色,码\) = [0-9]+' "$TMP/_ob_restore_$$.out" | grep -oE '[0-9]+$' | head -1)
assert_eq "${RESTORE_ORPHAN:-x}" "0" "恢复后孤儿对数回到 0"

# 证据留存（EVIDENCE_DIR 指向 docs/evidence/<卡>/ 时把每道闸的原始输出抄过去）
if [ -n "${EVIDENCE_DIR:-}" ]; then
    mkdir -p "$EVIDENCE_DIR"
    cp "$TMP/_ob_base_$$.out"    "$EVIDENCE_DIR/01_orphan_before.out"
    cp "$TMP/_pa_base_$$.out"    "$EVIDENCE_DIR/02_permission_audit_before.out"
    cp "$TMP/_pm_base_$$.out"    "$EVIDENCE_DIR/03_perm_menu_gate.out"
    cp "$TMP/_mi_base_$$.out"    "$EVIDENCE_DIR/04_menu_invariants.out"
    cp "$TMP/_ob_after_$$.out"   "$EVIDENCE_DIR/05_orphan_after.out"
    cp "$TMP/_pa_after_$$.out"   "$EVIDENCE_DIR/06_permission_audit_after.out"
    cp "$TMP/_ob_down_$$.out"    "$EVIDENCE_DIR/07_orphan_after_down.out"
    cp "$TMP/_ob_restore_$$.out" "$EVIDENCE_DIR/08_orphan_after_restore.out"
    echo "（证据已抄到 $EVIDENCE_DIR）"
fi

rm -f "$TMP"/_pm_base_$$.out "$TMP"/_ob_base_$$.out "$TMP"/_pa_base_$$.out "$TMP"/_mi_base_$$.out \
      "$TMP"/_ob_after_$$.out "$TMP"/_pm_after_$$.out "$TMP"/_pa_after_$$.out "$TMP"/_mi_after_$$.out \
      "$TMP"/_ob_down_$$.out "$TMP"/_ob_restore_$$.out

echo "=============================================================="
echo "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -eq 0 ]; then echo "ALL PASS ✅"; exit 0; else echo "有失败项 ❌"; exit 1; fi
