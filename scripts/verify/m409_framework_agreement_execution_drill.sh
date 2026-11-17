#!/usr/bin/env bash
# ============================================================================
# M4-09 框架协议「执行量统计」（已用 / 剩余）· 演练
#
# 卡：t_645358e9（M4-09【M4·采购】框架协议：执行量统计）
#
# 这一卡交的是**只读报表**（两个视图 + 两个只读接口 + 一行菜单按钮），所以脚本验五层：
#   1. 迁移：从模板库克隆演练库 → 推到 V0451 → 上 V0452 **连跑两遍**（幂等）
#   2. 结构：两个视图在位 + 关键列齐全；页面行 / 按钮行（父级 / 权限码 / 标题长度 / sort）；
#      采购经理与财务经理都已授权；**本卡新增路由 2 条、前端 0 处改动**
#   3. 口径（SQL 独立复核，不依赖 Rust）：存活单吃额度 / 作废与软删不吃 / 不限量剩余为 NULL /
#      含税协议按 with_tax 比 / 单头封顶优先 / 明细合计兜底
#   4. 行为（真库 Rust 用例，走真实建单入口）：12 条真库 + 12 条纯逻辑（含 5 条 dto 纯函数）
#   5. 回滚三段：down（视图 + 菜单 + 授权全撤）→ up 复原（**无数据护栏**：视图是纯派生物）
#
# 用法：
#   bash scripts/verify/m409_framework_agreement_execution_drill.sh                  # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m409_framework_agreement_execution_drill.sh # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m409_framework_agreement_execution_drill.sh         # 留库排查
#   M409_BAD_URL=1 bash scripts/verify/m409_framework_agreement_execution_drill.sh    # SKIP 守卫自检（期望报红）
#   SKIP_REPLAY=1 bash scripts/verify/m409_framework_agreement_execution_drill.sh     # 跳过空库全链重放
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 连接串密码一律复用 `$PGPASSWORD`（不在 URL 里写占位符 —— 那样真库用例会连不上、
# 打 SKIP 还记 ok，是假绿）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
MIG452="$MIG_DIR/V0452__framework_agreement_execution.up.sql"
DOWN452="$MIG_DIR/V0452__framework_agreement_execution.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M409_DB:-cp_m409_exec_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_REPLAY="${SKIP_REPLAY:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m409_drill_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

case "$DB" in
    *drill*|*e2e*|*test*) : ;;
    *) printf '库名 %s 不含 drill/e2e/test，拒绝在它上面跑（rc=2）\n' "$DB"; exit 2 ;;
esac

OWN_DB=1
if [ -n "${M409_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }

assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

accepts() { # sql 说明
    local out
    out="$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -v ON_ERROR_STOP=1 -c "$1" 2>&1)"
    if [ $? -eq 0 ]; then ok "$2"; else bad "$2 —— 被拒了：$(printf '%s' "$out" | head -2 | tr '\n' ' ')"; fi
}

cleanup() {
    if [ "$OWN_DB" = "0" ] || [ "$KEEP_DB" = "1" ]; then
        printf '\n  （库保留 → %s；不删）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG"
}
trap cleanup EXIT

printf '== M4-09 框架协议执行量统计演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

# ---------- 0) 模板与克隆 ----------
echo "== 0) 前置：模板库可克隆"
if ! psql_admin "SELECT 1 FROM pg_database WHERE datname = '$TEMPLATE_DB'" | grep -q 1; then
    bad "模板库 $TEMPLATE_DB 不存在（请先在两台机器的 PG 里准备模板库）"
    exit 2
fi
ok "模板库 $TEMPLATE_DB 存在"
if [ "$OWN_DB" = "1" ]; then
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    if psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE_DB" >/dev/null 2>&1; then
        ok "从模板克隆演练库 $DB"
    else
        bad "克隆演练库失败"
        exit 2
    fi
else
    ok "使用调用方给的演练库 $DB（只跑不清）"
fi

# ---------- 1) 推到 V0451，再上 V0452 ----------
echo "== 1) 补齐业务迁移 V0405…V0451（V0452 之前）"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    num="${f:1:4}"
    if [ "$num" -le 404 ]; then continue; fi
    if [ "$num" -ge 452 ]; then continue; fi
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条 V0405…V0451 迁移全部应用成功（演练库推到 V0451）"
fi

echo "== 1b) 应用 V0452（连跑两遍验幂等）"
MENU_BEFORE="$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.views WHERE table_schema='public' AND table_name LIKE 'v_framework_agreement%'")" \
    "上 V0452 之前框架协议 0 个视图（下面每条断言才说明是 V0452 建的）"
if psql_file "$MIG452"; then ok "第一遍应用成功"; else
    bad "第一遍应用失败：$(tail -3 "$LOG" | tr '\n' ' ')"; exit 2
fi
if psql_file "$MIG452"; then ok "第二遍应用成功（幂等：只该出 NOTICE）"; else
    bad "第二遍应用失败（幂等破了）：$(tail -3 "$LOG" | tr '\n' ' ')"
fi
if grep -q 'ERROR' "$LOG"; then
    bad "第二遍输出里出现 ERROR：$(grep 'ERROR' "$LOG" | head -2 | tr '\n' ' ')"
else
    ok "第二遍输出无 ERROR（CREATE OR REPLACE VIEW / ON CONFLICT 生效）"
fi
MENU_AFTER="$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
assert_eq "$((MENU_BEFORE + 2))" "$MENU_AFTER" "V0452 只加 2 行菜单（页面 + 执行量按钮）"

# ---------- 2) 结构断言 ----------
echo "== 2) 结构断言"
assert_eq "2" "$(psql_q "SELECT count(*) FROM information_schema.views WHERE table_schema='public' AND table_name IN ('v_framework_agreement_item_execution','v_framework_agreement_execution')")" \
    "两个视图都在位（物料行级 + 单头级）"
assert_eq "28" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='v_framework_agreement_item_execution'")" \
    "物料行视图 28 列"
assert_eq "48" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='v_framework_agreement_execution'")" \
    "单头视图 48 列"
for col in executed_qty booked_qty remaining_qty executed_amount_capped cap_amount exec_qty_rate exec_state; do
    if [ "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='v_framework_agreement_item_execution' AND column_name='$col'")" = "1" ]; then
        ok "物料行视图有列 $col"
    else
        bad "物料行视图缺列 $col"
    fi
done
for col in cap_qty cap_amount remaining_qty remaining_amount exec_qty_rate exec_amount_rate validity_state is_effective released_doc_count; do
    if [ "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='v_framework_agreement_execution' AND column_name='$col'")" = "1" ]; then
        ok "单头视图有列 $col"
    else
        bad "单头视图缺列 $col"
    fi
done

echo "== 2b) 菜单两行（页面行 + 按钮行）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004480'::uuid AND menu_type=2 AND auth_code='business:erp:framework-agreement:page' AND path='/business/erp/framework-agreement' AND parent_id='20000000-0000-0000-0000-000000000946'::uuid AND sort_order=39 AND status=1 AND deleted_at IS NULL")" \
    "页面行：显形码 / path / 父级采购管理 / sort=39 / 启用"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id='20000000-0000-0000-0000-000000004481'::uuid AND menu_type=3 AND auth_code='business:erp:framework-agreement:execution' AND parent_id='20000000-0000-0000-0000-000000004480'::uuid AND deleted_at IS NULL")" \
    "按钮行：接口码 / 父级 = 页面行（不是孤儿）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id IN ('20000000-0000-0000-0000-000000004480'::uuid,'20000000-0000-0000-0000-000000004481'::uuid) AND length(title) > 12")" \
    "两行 title 都 ≤12 字（侧栏不许换行）"
GRANT_N="$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id WHERE rm.menu_id IN ('20000000-0000-0000-0000-000000004480'::uuid,'20000000-0000-0000-0000-000000004481'::uuid) AND r.role_code IN ('PURCHASE_MANAGER','FINANCE_MANAGER')")"
if [ "$GRANT_N" = "0" ]; then
    info "模板库里没有 PURCHASE_MANAGER / FINANCE_MANAGER 角色 ⇒ 授权行 0（迁移的 INSERT…SELECT 按设计跳过）"
else
    ok "两个角色共拿到 $GRANT_N 条菜单授权"
fi

echo "== 2c) 本卡范围守卫（新增 2 条路由 / 前端 0 处改动）"
ROUTE_HITS="$(grep -c '#\[nik::get("/business/erp/framework-agreement' "$BACKEND/app/src/business/erp/purchase/framework_agreement_execution_handler.rs" || true)"
assert_eq "2" "$ROUTE_HITS" "执行量接口 2 条路由（总览 + 详情）"
PERM_HITS="$(grep -c 'permission = "business:erp:framework-agreement:execution"' "$BACKEND/app/src/business/erp/purchase/framework_agreement_execution_handler.rs" || true)"
assert_eq "2" "$PERM_HITS" "两条路由都挂了接口码注解（N16 裸路由门禁）"
FRONT_HITS="$(grep -rl 'framework-agreement' "$ROOT/frontend/src" 2>/dev/null | wc -l)"
assert_eq "0" "$FRONT_HITS" "前端 0 处改动（Vue 页面属 M4-11）"

# ---------- 3) 口径（SQL 独立复核） ----------
echo "== 3) 口径复核（SQL 直查视图，不依赖 Rust）"
psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -v ON_ERROR_STOP=1 >"$LOG" 2>&1 <<'SQL'
INSERT INTO biz_framework_agreement
    (id, agreement_no, agreement_name, supplier_id, supplier_name, start_date, end_date,
     price_mode, price_includes_tax, price_currency, price_tolerance,
     total_max_qty, total_max_amount, status)
VALUES ('00000000-0000-4000-8000-000045910001'::uuid, 'FWA-19990101-954101', 'M409-演练探针',
        gen_random_uuid(), '演练供应商', CURRENT_DATE - 5, CURRENT_DATE + 5,
        'FIXED', FALSE, 'CNY', 0, 200, 5000, 'APPROVED');
INSERT INTO biz_framework_agreement_item
    (id, agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate, max_qty)
VALUES ('00000000-0000-4000-8000-000045910002'::uuid, '00000000-0000-4000-8000-000045910001'::uuid,
        1, gen_random_uuid(), gen_random_uuid(), 10, 0.13, 100),
       ('00000000-0000-4000-8000-000045910003'::uuid, '00000000-0000-4000-8000-000045910001'::uuid,
        2, gen_random_uuid(), gen_random_uuid(), 5, 0.13, NULL);
INSERT INTO biz_purchase_order
    (id, order_no, supplier_id, order_date, warehouse_id, status, framework_agreement_id)
VALUES ('00000000-0000-4000-8000-000045910011'::uuid, 'PO-19990101-954101', gen_random_uuid(),
        CURRENT_DATE, gen_random_uuid(), 'APPROVED', '00000000-0000-4000-8000-000045910001'::uuid),
       ('00000000-0000-4000-8000-000045910012'::uuid, 'PO-19990101-954102', gen_random_uuid(),
        CURRENT_DATE, gen_random_uuid(), 'CANCELLED', '00000000-0000-4000-8000-000045910001'::uuid),
       ('00000000-0000-4000-8000-000045910013'::uuid, 'PO-19990101-954103', gen_random_uuid(),
        CURRENT_DATE, gen_random_uuid(), 'DRAFT', '00000000-0000-4000-8000-000045910001'::uuid);
UPDATE biz_purchase_order SET deleted_at = now() WHERE id = '00000000-0000-4000-8000-000045910013'::uuid;
INSERT INTO biz_purchase_order_item
    (order_id, line_no, material_id, unit_id, qty, price, amount, tax_amount, with_tax_amount, framework_agreement_item_id)
VALUES ('00000000-0000-4000-8000-000045910011'::uuid, 1, gen_random_uuid(), gen_random_uuid(),
        30, 10, 300, 39, 339, '00000000-0000-4000-8000-000045910002'::uuid),
       ('00000000-0000-4000-8000-000045910011'::uuid, 2, gen_random_uuid(), gen_random_uuid(),
        10, 5, 50, 6.5, 56.5, '00000000-0000-4000-8000-000045910003'::uuid),
       ('00000000-0000-4000-8000-000045910012'::uuid, 1, gen_random_uuid(), gen_random_uuid(),
        20, 10, 200, 26, 226, '00000000-0000-4000-8000-000045910002'::uuid),
       ('00000000-0000-4000-8000-000045910013'::uuid, 1, gen_random_uuid(), gen_random_uuid(),
        5, 10, 50, 6.5, 56.5, '00000000-0000-4000-8000-000045910002'::uuid);
SQL
if [ $? -eq 0 ]; then ok "探针数据（1 协议 / 2 物料行 / 3 采购单：存活 + 作废 + 软删）插入成功"; else
    bad "探针数据插入失败：$(tail -3 "$LOG" | tr '\n' ' ')"; fi

assert_eq "1" "$(psql_q "SELECT (executed_qty = 30 AND remaining_qty = 70 AND exec_qty_rate = 0.3)::int FROM v_framework_agreement_item_execution WHERE agreement_item_id='00000000-0000-4000-8000-000045910002'::uuid")" \
    "限量行：已用 30（作废 20 / 软删 5 都不算）、剩余 70、率 0.3"
assert_eq "PARTIAL" "$(psql_q "SELECT exec_state FROM v_framework_agreement_item_execution WHERE agreement_item_id='00000000-0000-4000-8000-000045910002'::uuid")" \
    "限量行状态 PARTIAL"
assert_eq "1" "$(psql_q "SELECT (executed_qty = 10 AND remaining_qty IS NULL AND exec_qty_rate IS NULL AND cap_amount IS NULL)::int FROM v_framework_agreement_item_execution WHERE agreement_item_id='00000000-0000-4000-8000-000045910003'::uuid")" \
    "不限量行：剩余 / 率 / 金额上限都是 NULL（不是 0）"
assert_eq "UNLIMITED" "$(psql_q "SELECT exec_state FROM v_framework_agreement_item_execution WHERE agreement_item_id='00000000-0000-4000-8000-000045910003'::uuid")" \
    "不限量行状态 UNLIMITED"
assert_eq "1" "$(psql_q "SELECT (executed_qty = 40 AND executed_amount = 350 AND cap_qty = 200 AND remaining_qty = 160 AND exec_qty_rate = 0.2)::int FROM v_framework_agreement_execution WHERE agreement_id='00000000-0000-4000-8000-000045910001'::uuid")" \
    "单头：已用 40 / 金额 350 / 封顶 200（单头优先）/ 剩余 160 / 率 0.2"
assert_eq "1" "$(psql_q "SELECT (order_doc_count = 1 AND released_doc_count = 2)::int FROM v_framework_agreement_execution WHERE agreement_id='00000000-0000-4000-8000-000045910001'::uuid")" \
    "单头：计入 1 张 / 已释放 2 张（作废 + 软删）"
assert_eq "1" "$(psql_q "SELECT (validity_state = 'ACTIVE' AND is_effective AND days_left >= 0)::int FROM v_framework_agreement_execution WHERE agreement_id='00000000-0000-4000-8000-000045910001'::uuid")" \
    "单头：APPROVED + 今天在效期内 ⇒ ACTIVE / is_effective"
assert_eq "PO-19990101-954102" "$(psql_q "SELECT o.order_no FROM biz_purchase_order_item i JOIN biz_purchase_order o ON o.id=i.order_id WHERE i.framework_agreement_item_id='00000000-0000-4000-8000-000045910002'::uuid AND o.status='CANCELLED'")" \
    "作废单仍在采购明细里（视图只是不计入，不是删数据）"

echo "== 3b) 探针清场"
psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -c \
    "DELETE FROM biz_purchase_order WHERE id IN ('00000000-0000-4000-8000-000045910011'::uuid,'00000000-0000-4000-8000-000045910012'::uuid,'00000000-0000-4000-8000-000045910013'::uuid);
     DELETE FROM biz_framework_agreement WHERE id='00000000-0000-4000-8000-000045910001'::uuid;" >/dev/null 2>&1
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement WHERE agreement_name = 'M409-演练探针'")" \
    "演练探针协议 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'PO-19990101-9541%'")" \
    "演练探针采购单 0 残留"

# ---------- 4) 空库全链重放 ----------
echo "== 4) 空库全链重放（mig_replay_fresh.sh，独立库）"
if [ "$SKIP_REPLAY" = "1" ]; then
    info "SKIP_REPLAY=1：跳过（本地快速迭代用；提交前必须跑全）"
else
    REPLAY_OUT="$TMP/m409_$$_replay.out"
    if DBHOST="$PG_HOST" DBPORT="$PG_PORT" DBUSER="$PG_USER" DBPASS="$PGPASSWORD" DBOWNER="$PG_USER" \
       bash "$ROOT/scripts/verify/mig_replay_fresh.sh" "m409$$" >"$REPLAY_OUT" 2>&1; then
        ok "空库按版本号升序重放全链通过（$(grep -oE '[0-9]+/[0-9]+ 条版本化迁移[^ ]*' "$REPLAY_OUT" | tail -1)）"
    else
        bad "空库重放失败（见 $REPLAY_OUT）"
        grep -E '✗ 失败|ERROR|== 结果' "$REPLAY_OUT" | head -6 | sed 's/^/        /'
    fi
fi

# ---------- 5) 回滚三段 ----------
echo "== 5) 回滚（视图 + 菜单 + 授权全撤 → 再 up 复原）"
if psql_file "$DOWN452"; then ok "down 成功（视图是纯派生物，无数据护栏）"; else
    bad "down 失败：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.views WHERE table_schema='public' AND table_name IN ('v_framework_agreement_item_execution','v_framework_agreement_execution')")" \
    "down 后两个视图已删"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE id IN ('20000000-0000-0000-0000-000000004480'::uuid,'20000000-0000-0000-0000-000000004481'::uuid)")" \
    "down 后两行菜单已删"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_role_menu WHERE menu_id IN ('20000000-0000-0000-0000-000000004480'::uuid,'20000000-0000-0000-0000-000000004481'::uuid)")" \
    "down 后授权行已删"
if psql_file "$MIG452"; then ok "再 up 复原成功"; else bad "再 up 失败：$(tail -3 "$LOG" | tr '\n' ' ')"; fi
assert_eq "2" "$(psql_q "SELECT count(*) FROM information_schema.views WHERE table_schema='public' AND table_name IN ('v_framework_agreement_item_execution','v_framework_agreement_execution')")" \
    "up 复原后两个视图回来了"

# ---------- 6) 真库 Rust 用例（+ SKIP 有牙自检） ----------
echo "== 6) 真库用例（--test-threads=1）"
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
if [ "${M409_BAD_URL:-}" = "1" ]; then
    export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/no_such_db_skip_guard"
    export SYS_DB_URL="$ERP_DB_URL"
    info "SKIP 自检模式（M409_BAD_URL=1）：连接串故意写错，期望 SKIP>0"
fi

OUT="$TMP/m409_$$_tests.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib business::erp::purchase::framework_agreement_execution -- --test-threads=1 --nocapture ) >"$OUT" 2>&1
LINE="$(grep -E '^test result: ' "$OUT" | tail -1)"
PASSED="$(printf '%s' "$LINE" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
FAILED="$(printf '%s' "$LINE" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
SKIPPED="$(grep -c 'SKIP:' "$OUT" || true)"
[ -z "${PASSED:-}" ] && PASSED=0
[ -z "${FAILED:-}" ] && FAILED=0
[ -z "${SKIPPED:-}" ] && SKIPPED=0
if [ "$FAILED" -eq 0 ] && [ "$PASSED" -ge 24 ]; then
    ok "本卡用例全绿（$PASSED 条：12 纯逻辑 + 12 真库）"
elif [ "$FAILED" -ne 0 ]; then
    bad "本卡用例有 $FAILED 条失败（见 $OUT）"
    grep -E 'panicked at|assertion|left:|right:' -A 2 "$OUT" | head -30 | sed 's/^/        /'
else
    bad "本卡用例只跑了 $PASSED 条（期望 ≥ 24，见 $OUT）"
fi
if [ "$SKIPPED" -eq 0 ]; then ok "本组 SKIP=0（真库用例没被「连不上」打成假绿）"; else
    bad "本组出现 $SKIPPED 行 SKIP（假绿）"; grep 'SKIP:' "$OUT" | head -5 | sed 's/^/        /'
fi
POS=0
for marker in '新协议' '累加' '作废' '软删' '超限' '不限量' '单头封顶' '金额口径' '合计对拍' '有效期' '总览筛选' '清场'; do
    if grep -qF "[M4-09] $marker" "$OUT"; then POS=$((POS+1)); else bad "缺正向标记：[M4-09] $marker"; fi
done
if [ "$POS" -eq 12 ]; then ok "正向标记 12/12 在位（每条用例都真跑过）"; fi

# ---------- 7) 门禁：权限审计 + 菜单结构 ----------
echo "== 7) 门禁（permission_audit_tests + menu_invariants.sh）"
AUDIT_OUT="$TMP/m409_$$_audit.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib system::menu::permission_audit_tests -- --test-threads=1 ) >"$AUDIT_OUT" 2>&1
AUDIT_LINE="$(grep -E '^test result: ' "$AUDIT_OUT" | tail -1)"
if printf '%s' "$AUDIT_LINE" | grep -qE 'test result: ok\.' && ! printf '%s' "$AUDIT_LINE" | grep -qE '[1-9][0-9]* failed'; then
    ok "permission_audit_tests 全绿（页面码已登记 ROUTE_LEVEL_ONLY，接口码有注解）"
else
    bad "permission_audit_tests 不是全绿：${AUDIT_LINE:-无输出}（见 $AUDIT_OUT）"
fi
if PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" >"$TMP/m409_$$_menu.out" 2>&1; then
    ok "menu_invariants.sh 8 条断言全绿"
else
    bad "menu_invariants.sh 报红（见 $TMP/m409_$$_menu.out）"
    tail -15 "$TMP/m409_$$_menu.out" | sed 's/^/        /'
fi
info "菜单总数：$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")（门禁区间 400-560；本卡 +2）"

# ---------- 8) 夹具残留 ----------
echo "== 8) 夹具残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement WHERE agreement_name LIKE 'M409-%'")" "M409- 协议 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE remark LIKE 'M409-%'")" "M409- 采购单 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'PO-19990101-9541%'")" "演练探针采购单 0 残留"

# ---------- 9) 汇总 ----------
echo
if [ "$FAIL" -eq 0 ]; then
    printf '\033[32m== M4-09 演练结果：PASS=%s  FAIL=0 —— 框架协议执行量统计（已用 / 剩余 · 两个只读接口）就绪\033[0m\n' "$PASS"
    exit 0
else
    printf '\033[31m== M4-09 演练结果：PASS=%s  FAIL=%s —— 见上面红行\033[0m\n' "$PASS" "$FAIL"
    exit 1
fi
