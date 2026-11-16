#!/usr/bin/env bash
# ============================================================================
# M2-05 在途库存对账（在途 + 已出 = 发货总量）· 真库演练
#
# 卡：t_b545b37a（M2-05【M2·仓库】在途库存：对账（在途 + 已出 = 发货总量））
# 上游：t_f1bce846（M2-04 查询）/ t_5f324a4c（M2-03 签收）/ t_6c090f97（M2-02 过账）
#       t_7ba5b0e2（M2-01 表 biz_inventory_in_transit）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 先补模板落后的业务迁移 V0405…V0421（V0421 = 在途读码），**V0422 留到第 4 步**
#   3. 【基线】在「还没有 V0422」的库上跑三道闸，把别人的既有红记下来：
#        · perm_menu_gate.py  → 必须**报出本卡的对账码**（证闸是活的：没有迁移就授不出去）
#        · permission_audit_tests（N14…N21）→ 记下失败用例集
#        · orphan_button_gate.py → 记下孤儿 (角色,码) 清单
#   4. 应用 V0422（对账权限码），硬断言：码只有 1 个承载节点、menu_type=3、path 为空
#      （不进侧栏）、挂在「即时库存」页下、sort_order=30、持在途读码的角色都拿到它
#   5. 幂等（up 再跑）+ 回滚（down → down 再跑 → up 恢复，授权不重复，且不带走 V0421 的码）
#   6. 真库用例（--test-threads=1）：
#        business::erp::inventory_transit   ← 本卡主证据（对账真库 10 条 + 查询 19 条 + 纯逻辑）
#        business::erp::shipment            ← 上游写路径未被破坏（在途账写入口）
#      并检查 SKIP=0：`ERP_DB_URL` 写漏 / 掩码会把真库用例全打成 SKIP 而"全绿"
#   7. 【收尾】再跑一遍三道闸，与本迁移前的基线**逐条比对**：
#        · perm_menu_gate 必须转绿（本卡的码有承载了）
#        · permission_audit 失败集必须与基线**一模一样**（0 新增红）
#        · orphan 清单必须与基线**一模一样**（0 新增孤儿）
#        · menu_invariants.sh 8 条结构不变量必须全绿
#   8. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m205_transit_reconcile_drill.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m205_transit_reconcile_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m205_transit_reconcile_drill.sh          # 留库排查（打印库名）
#   SKIP_GATES=1 …                                                         # 只跑迁移 + Rust 用例
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
APP_DIR="$ROOT/backend/app"
MIG_UP="$MIG_DIR/V0422__erp_inventory_transit_reconcile.up.sql"
MIG_DOWN="$MIG_DIR/V0422__erp_inventory_transit_reconcile.down.sql"
MY_CODE="business:erp:inventory-transit:audit"
LIST_CODE="business:erp:inventory-transit:list"
MY_MENU_ID="20000000-0000-0000-0000-000000004220"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m205_reconcile_drill_$$"
KEEP_DB="${KEEP_DB:-0}"
SKIP_GATES="${SKIP_GATES:-0}"

TMP="${TMPDIR:-/tmp}"
LOG="$TMP/m205_drill_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }
assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}
assert_ge() { # 下限 实际 说明
    if [ "${2:-0}" -ge "$1" ] 2>/dev/null; then ok "$3（=$2）"; else bad "$3（期望 ≥ $1，实际 [$2]）"; fi
}

cleanup() {
    if [ "$KEEP_DB" = "1" ]; then
        printf '\n  （KEEP_DB=1：演练库保留 → %s）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG"
}
trap cleanup EXIT

export NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT="$PG_PORT"
export NIK_DATABASE_USER="$PG_USER" NIK_DATABASE_NAME="$DB" NIK_DATABASE_PASSWORD="$PGPASSWORD"

# ---- 闸 / 用例的解析器 ----
perm_gate_run()     { ( cd "$ROOT" && python3 scripts/verify/perm_menu_gate.py ) >"$1" 2>&1; }
orphan_gate_run()   { ( cd "$ROOT" && python3 scripts/verify/orphan_button_gate.py ) >"$1" 2>&1; }
# 孤儿 (角色,码) 清单（"  · ROLE code 不可达父页 …" 取前两列）
orphan_pairs()      { grep -E '^  · ' "$1" 2>/dev/null | sed 's/^  · //' | awk '{print $1" "$2}' | sort -u; }
# permission_audit 失败用例清单
audit_run()         { ( cd "$APP_DIR" && cargo test --lib system::menu::permission_audit_tests -- --test-threads=1 ) >"$1" 2>&1; }
audit_failures()    { grep -E '^test .* FAILED$' "$1" 2>/dev/null | sed 's/ \.\.\. FAILED$//' | sort -u; }
# N14 报出的死码清单
audit_dead_codes()  { sed -n '/0 落地/,/修法/p' "$1" 2>/dev/null | grep -E '^[a-z][a-z0-9_-]*:[a-z0-9:_-]+$' | sort -u; }

printf '== M2-05 在途库存对账 · 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

# ---------- 0) 模板与前置 ----------
echo "== 0) 前置：模板库可克隆"
if ! psql_admin "SELECT 1 FROM pg_database WHERE datname = '$TEMPLATE_DB'" | grep -q 1; then
    bad "模板库 $TEMPLATE_DB 不存在（请先在两台机器的 PG 里准备模板库）"
    exit 1
fi
ok "模板库 $TEMPLATE_DB 存在"

psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
if psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE_DB" >/dev/null 2>&1; then
    ok "从模板克隆演练库 $DB"
else
    bad "克隆演练库失败"
    exit 1
fi

# ---------- 1) 补齐迁移 V0405…V0421（**不含** V0422） ----------
echo "== 1) 补齐业务迁移 V0405…V0421（幂等，逐条 ON_ERROR_STOP）"
MIG_FAIL=0
APPLIED=0
for n in 05 06 07 08 09 10 11 12 13 14 15 16 17 18 19 20 21; do
    for f in "$MIG_DIR"/V04${n}__*.up.sql; do
        [ -e "$f" ] || continue
        APPLIED=$((APPLIED+1))
        if ! psql_file "$f"; then
            MIG_FAIL=$((MIG_FAIL+1))
            bad "迁移失败：$(basename "$f")（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
        fi
    done
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条业务迁移全部应用成功（V0420 建在途账 / V0421 在途读码）"
fi
assert_eq "biz_inventory_in_transit" "$(psql_q "SELECT to_regclass('public.biz_inventory_in_transit')")" "在途账表存在"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$LIST_CODE' AND deleted_at IS NULL")" "上游在途读码 V0421 已就位（1 个承载节点）"

# ---------- 2) 基线：迁移前的闸状态 ----------
# ⚠ 数据库 URL 必须**在基线之前**就导出：基线要跑 permission_audit（真库用例），
# 先跑后导会让它打一排 SKIP 然后「0 失败」= 假绿。
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"

BASE_PERM="$TMP/m205_$$_perm_before.out"
BASE_AUDIT="$TMP/m205_$$_audit_before.out"
BASE_ORPH="$TMP/m205_$$_orphan_before.out"
BASE_AUDIT_FAILS="$TMP/m205_$$_audit_before.fails"
BASE_ORPHANS="$TMP/m205_$$_orphan_before.pairs"
BASE_DEAD="$TMP/m205_$$_audit_before.dead"

echo "== 2) 基线（还没有 V0422）：记下别人的既有红"
if perm_gate_run "$BASE_PERM"; then
    info "perm_menu_gate 在迁移前就绿了（本卡的码还没进菜单却没被报出来 —— 闸的扫描面要复核）"
else
    if grep -q "$MY_CODE" "$BASE_PERM"; then
        ok "闸是活的：没有 V0422 时 perm_menu_gate 报出本卡的对账码（授不出去）"
    else
        bad "迁移前 perm_menu_gate 没报出 $MY_CODE（死权限码闸对本卡不设防）"
    fi
fi
audit_run "$BASE_AUDIT"
audit_failures "$BASE_AUDIT" > "$BASE_AUDIT_FAILS"
audit_dead_codes "$BASE_AUDIT" > "$BASE_DEAD"
info "基线 permission_audit 失败用例 $(wc -l < "$BASE_AUDIT_FAILS") 条 / 死码 $(wc -l < "$BASE_DEAD") 个：$(paste -sd' ' "$BASE_DEAD")"
if orphan_gate_run "$BASE_ORPH"; then
    info "基线 orphan_button_gate 全绿（无既有孤儿）"
fi
orphan_pairs "$BASE_ORPH" > "$BASE_ORPHANS"
info "基线孤儿 (角色,码) $(wc -l < "$BASE_ORPHANS") 对"

# ---------- 3) 应用 V0422 + 权限码形状 ----------
echo "== 3) 应用 V0422，检查权限码落库形状"
if psql_file "$MIG_UP"; then
    ok "V0422 应用成功（对账权限码已种）"
else
    bad "V0422 应用失败（tail: $(tail -3 "$LOG" | tr '\n' ' ')）"
fi
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$MY_CODE' AND deleted_at IS NULL")" "码只有 1 个承载节点"
assert_eq "3" "$(psql_q "SELECT menu_type FROM sys_menu WHERE id = '$MY_MENU_ID'::uuid AND deleted_at IS NULL")" "承载节点是按钮（menu_type=3，不进侧栏）"
assert_eq "" "$(psql_q "SELECT COALESCE(path, '') FROM sys_menu WHERE id = '$MY_MENU_ID'::uuid AND deleted_at IS NULL")" "按钮无 path（侧栏不会多出点不通的入口）"
assert_eq "即时库存" "$(psql_q "SELECT p.title FROM sys_menu c JOIN sys_menu p ON p.id = c.parent_id WHERE c.id = '$MY_MENU_ID'::uuid")" "挂在「即时库存」页面下"
assert_eq "30" "$(psql_q "SELECT sort_order FROM sys_menu WHERE id = '$MY_MENU_ID'::uuid AND deleted_at IS NULL")" "同级 sort_order = 30（巡检 10 / 在途查询 20 / 对账 30）"
assert_eq "0" "$(psql_q "
    SELECT count(*) FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id
                    AND rm.menu_id IN (SELECT id FROM sys_menu WHERE auth_code = '$LIST_CODE' AND deleted_at IS NULL))
       AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id
                    AND rm.menu_id = '$MY_MENU_ID'::uuid)")" \
    "持在途读码的角色都拿到了对账码（缺口 0）"
assert_ge "1" "$(psql_q "SELECT count(DISTINCT rm.role_id) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id WHERE rm.menu_id = '$MY_MENU_ID'::uuid AND r.role_code <> 'SUPER_ADMIN'")" \
    "至少 1 个**非超管**角色拿到码（N20 口径）"

# ---------- 4) 幂等 / 回滚 ----------
echo "== 4) 幂等（up 再跑一次）与回滚（down → down → up）"
MENU_BEFORE="$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$MY_CODE'")"
GRANT_BEFORE="$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id = '$MY_MENU_ID'::uuid")"
if psql_file "$MIG_UP"; then ok "V0422 重放（幂等）执行成功"; else bad "V0422 重放失败（tail: $(tail -3 "$LOG" | tr '\n' ' ')）"; fi
assert_eq "$MENU_BEFORE" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$MY_CODE'")" "重放后承载节点数不变"
assert_eq "$GRANT_BEFORE" "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id = '$MY_MENU_ID'::uuid")" "重放后授权行数不变（不重复授）"

if psql_file "$MIG_DOWN"; then ok "V0422 down 执行成功"; else bad "V0422 down 失败（tail: $(tail -3 "$LOG" | tr '\n' ' ')）"; fi
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$MY_CODE'")" "down 后码清零"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id = '$MY_MENU_ID'::uuid")" "down 后授权行清零"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$LIST_CODE' AND deleted_at IS NULL")" "down 没带走 V0421 的在途读码"
if psql_file "$MIG_DOWN"; then ok "down 可重放（幂等）"; else bad "down 重放失败"; fi
if psql_file "$MIG_UP"; then ok "V0422 重新 up 成功"; else bad "V0422 重新 up 失败"; fi
assert_eq "$GRANT_BEFORE" "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id = '$MY_MENU_ID'::uuid")" "重新 up 后授权行恢复且不重复"

# ---------- 5) 真库用例 ----------
echo "== 5) 真库用例（--test-threads=1）"
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m205_$$_${module//[:/]/_}.out"
    ( cd "$APP_DIR" && cargo test --lib "$module" -- --test-threads=1 ) >"$out" 2>&1
    line="$(grep -E '^test result: ' "$out" | head -1)"
    passed="$(printf '%s' "$line" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
    skipped="$(grep -c '^SKIP' "$out" || true)"
    [ -z "${passed:-}" ] && passed=0
    [ -z "${failed:-}" ] && failed=0
    [ -z "${skipped:-}" ] && skipped=0
    if [ "$failed" -eq 0 ]; then
        assert_ge "$min" "$passed" "$label：$module 全绿（$passed 条）"
    else
        bad "$label：$module 有 $failed 条失败（见 $out）"
    fi
    if [ "$skipped" -eq 0 ]; then
        ok "$label：本组 SKIP=0（真库用例没有被「无 ERP_DB_URL」打成假绿）"
    else
        bad "$label：本组出现 $skipped 行 SKIP（真库用例被跳过 = 假绿）"
    fi
}
run_group "business::erp::inventory_transit" 41 "本卡（真库 18 条：对账 10 + 查询 8；纯逻辑 23 条）"
run_group "business::erp::shipment" 40 "上游发货域（在途写入口未被破坏）"

# ---------- 6) 收尾：三道闸与基线逐条比对 ----------
if [ "$SKIP_GATES" = "1" ]; then
    info "SKIP_GATES=1：跳过收尾的三道闸"
else
    echo "== 6) 收尾：三道闸 vs 基线"
    AFTER_PERM="$TMP/m205_$$_perm_after.out"
    AFTER_AUDIT="$TMP/m205_$$_audit_after.out"
    AFTER_ORPH="$TMP/m205_$$_orphan_after.out"
    AFTER_AUDIT_FAILS="$TMP/m205_$$_audit_after.fails"
    AFTER_ORPHANS="$TMP/m205_$$_orphan_after.pairs"
    AFTER_DEAD="$TMP/m205_$$_audit_after.dead"

    if perm_gate_run "$AFTER_PERM"; then
        ok "perm_menu_gate 转绿：本卡的码有菜单承载（注解 ↔ 菜单一一对应）"
    else
        bad "perm_menu_gate 仍红（tail: $(tail -3 "$AFTER_PERM" | tr '\n' ' ')）"
    fi

    audit_run "$AFTER_AUDIT"
    audit_failures "$AFTER_AUDIT" > "$AFTER_AUDIT_FAILS"
    audit_dead_codes "$AFTER_AUDIT" > "$AFTER_DEAD"
    if grep -qx "$MY_CODE" "$AFTER_DEAD"; then
        bad "$MY_CODE 被判为死码（菜单有、源码没落地）"
    else
        ok "$MY_CODE 在源码里有落地（N14 不报它）"
    fi
    if diff -q "$BASE_AUDIT_FAILS" "$AFTER_AUDIT_FAILS" >/dev/null; then
        ok "permission_audit 失败集与迁移前**完全一致**（0 新增红；既有红 $(wc -l < "$BASE_AUDIT_FAILS") 条属别的卡）"
    else
        bad "permission_audit 失败集变了：新增 $(comm -13 "$BASE_AUDIT_FAILS" "$AFTER_AUDIT_FAILS" | paste -sd' ')"
    fi
    assert_eq "0" "$(grep -c "$MY_CODE" "$AFTER_AUDIT" || true)" "N14/N20 报文里没有本卡的码（无死码 / 已授非超管）"

    orphan_gate_run "$AFTER_ORPH" || true
    orphan_pairs "$AFTER_ORPH" > "$AFTER_ORPHANS"
    if grep -q "$MY_CODE" "$AFTER_ORPH"; then
        bad "orphan_button_gate 把本卡的码判成孤儿按钮"
    else
        ok "本卡的码不是孤儿按钮（授权角色的父页码都在）"
    fi
    if diff -q "$BASE_ORPHANS" "$AFTER_ORPHANS" >/dev/null; then
        ok "孤儿清单与迁移前**完全一致**（0 新增孤儿；既有 $(wc -l < "$BASE_ORPHANS") 对属别的卡）"
    else
        bad "孤儿清单变了：新增 $(comm -13 "$BASE_ORPHANS" "$AFTER_ORPHANS" | paste -sd' | ')"
    fi

    if ( cd "$ROOT" && bash scripts/verify/menu_invariants.sh "$DB" ) >"$TMP/m205_$$_menu.out" 2>&1; then
        ok "menu_invariants：8 条菜单结构不变量全绿"
    else
        bad "menu_invariants 未通过（tail: $(tail -5 "$TMP/m205_$$_menu.out" | tr '\n' ' ')）"
    fi
fi

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
