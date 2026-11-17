#!/usr/bin/env bash
# ============================================================================
# M4-08 框架协议「订单引用」（校验价格 / 数量 · 超限拒绝）· 演练
#
# 卡：t_06b8c65b（M4-08【M4·采购】框架协议：订单引用（校验价格/数量））
#
# 这一卡交的是**下单路径的校验与回写**，所以脚本验四层：
#   1. 迁移：从模板库克隆演练库 → 推到 V0450 → 上 V0451 **连跑两遍**（幂等）
#   2. 结构：三列可空 + 两个部分索引 + **无外键**（弱引用：协议软删/删除不该拦历史单）
#      + 存量行一个都没被改动 + 菜单/权限码/路由 0 新增（本卡不夹带）
#   3. 行为（真库 Rust 用例，走真实建单入口）：价格超容差 409/66097、
#      数量/金额超上限 409/66098、草稿与过期协议 400/66096、供应商与物料范围、
#      含税协议按税率折算后比价、改单重算、作废/删除释放额度、详情出参
#   4. 回滚三段：有引用 → 拒；清空 → 成功；再 up → 复原
#
# 用法：
#   bash scripts/verify/m408_framework_agreement_order_ref_drill.sh                 # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m408_framework_agreement_order_ref_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m408_framework_agreement_order_ref_drill.sh        # 留库排查
#   M408_BAD_URL=1 bash scripts/verify/m408_framework_agreement_order_ref_drill.sh   # SKIP 守卫自检（期望报红）
#   SKIP_REPLAY=1 bash scripts/verify/m408_framework_agreement_order_ref_drill.sh    # 跳过空库全链重放
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 连接串密码一律复用 `$PGPASSWORD`（不在 URL 里写占位符 —— 那样真库用例会连不上、
# 打 SKIP 还记 ok，是假绿）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
MIG450="$MIG_DIR/V0450__framework_agreement.up.sql"
MIG="$MIG_DIR/V0451__framework_agreement_order_ref.up.sql"
DOWN="$MIG_DIR/V0451__framework_agreement_order_ref.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M408_DB:-cp_m408_ref_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_REPLAY="${SKIP_REPLAY:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m408_drill_$$.log"
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
if [ -n "${M408_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }
psql_sql()   { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f - >"$LOG" 2>&1; }

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

printf '== M4-08 框架协议订单引用演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 推到 V0450，再上 V0451 ----------
echo "== 1) 补齐业务迁移 V0405…V0449 → 上 V0450（协议两表）"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    num="${f:1:4}"
    if [ "$num" -le 404 ]; then continue; fi
    if [ "$num" -ge 450 ]; then continue; fi
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条 V0405…V0449 迁移全部应用成功（演练库推到 V0449）"
fi
if psql_file "$MIG450"; then ok "V0450（协议表结构）应用成功"; else
    bad "V0450 失败：$(tail -3 "$LOG" | tr '\n' ' ')"; exit 2
fi

echo "== 1b) 应用 V0451（连跑两遍验幂等）"
MENU_BEFORE="$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name IN ('biz_purchase_order','biz_purchase_order_item') AND column_name LIKE 'framework_agreement%'")" \
    "上 V0451 之前订单表 0 个协议列（下面每条断言才说明是 V0451 加的）"
if psql_file "$MIG"; then ok "第一遍应用成功"; else
    bad "第一遍应用失败：$(tail -3 "$LOG" | tr '\n' ' ')"; exit 2
fi
if psql_file "$MIG"; then ok "第二遍应用成功（幂等：只该出 NOTICE）"; else
    bad "第二遍应用失败（幂等破了）：$(tail -3 "$LOG" | tr '\n' ' ')"
fi
if grep -q 'ERROR' "$LOG"; then
    bad "第二遍输出里出现 ERROR：$(grep 'ERROR' "$LOG" | head -2 | tr '\n' ' ')"
else
    ok "第二遍输出无 ERROR（ADD COLUMN / CREATE INDEX IF NOT EXISTS 生效）"
fi
MENU_AFTER="$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
assert_eq "$MENU_BEFORE" "$MENU_AFTER" "V0451 一条菜单都没加（上迁移前后菜单总数不变）"

# ---------- 2) 结构断言 ----------
echo "== 2) 结构断言"
assert_eq "3" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND ((table_name='biz_purchase_order' AND column_name IN ('framework_agreement_id','framework_agreement_no_snapshot')) OR (table_name='biz_purchase_order_item' AND column_name='framework_agreement_item_id'))")" \
    "三个引用列都在（单头 2 + 明细 1）"
assert_eq "3" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND ((table_name='biz_purchase_order' AND column_name IN ('framework_agreement_id','framework_agreement_no_snapshot')) OR (table_name='biz_purchase_order_item' AND column_name='framework_agreement_item_id')) AND is_nullable='YES'")" \
    "三个引用列都可空（NULL = 不挂协议；写成 NOT NULL 等于「没有协议不许采购」）"
assert_eq "2" "$(psql_q "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND indexname IN ('idx_biz_po_fwa','idx_biz_poi_fwa_item') AND indexdef LIKE '%WHERE%IS NOT NULL%'")" \
    "两个引用索引都是部分索引（存量零星采购不付索引代价）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.constraint_column_usage c JOIN information_schema.table_constraints t ON t.constraint_name=c.constraint_name WHERE t.constraint_type='FOREIGN KEY' AND c.table_name IN ('biz_purchase_order','biz_purchase_order_item') AND c.column_name LIKE 'framework_agreement%'")" \
    "引用列上没有外键（弱引用：协议软删/删除不许拦住历史采购单）"
assert_eq "1" "$(psql_q "SELECT (count(*) > 180)::int FROM information_schema.tables WHERE table_schema='public'")" "模板库克隆完整（public 表 > 180 张）"

echo "== 2b) 弱引用必须放行（协议行不存在也能插；下面立刻删掉探针）"
accepts "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, framework_agreement_id, framework_agreement_no_snapshot) VALUES ('00000000-0000-4000-8000-000004090001'::uuid, 'PO-19990101-900451', gen_random_uuid(), DATE '1999-01-01', gen_random_uuid(), gen_random_uuid(), 'FWA-19990101-900451')" \
    "采购单可以引用一个不存在的协议 id（弱引用无外键，历史单不因协议被删而失效）"
accepts "INSERT INTO biz_purchase_order_item (id, order_id, line_no, material_id, unit_id, qty, price, framework_agreement_item_id) VALUES ('00000000-0000-4000-8000-000004090002'::uuid, '00000000-0000-4000-8000-000004090001'::uuid, 1, gen_random_uuid(), gen_random_uuid(), 1, 1, gen_random_uuid())" \
    "明细可以引用一个不存在的协议行 id"
assert_eq "0" "$(psql_q "DELETE FROM biz_purchase_order WHERE id='00000000-0000-4000-8000-000004090001'::uuid" >/dev/null 2>&1; psql_q "SELECT count(*) FROM biz_purchase_order_item WHERE order_id='00000000-0000-4000-8000-000004090001'::uuid")" \
    "删单连带删明细（探针已清干净）"

# ---------- 3) 本卡不许夹带：权限码 0 / 菜单 0 / 路由 0 ----------
echo "== 3) 本卡范围守卫（不夹带权限码 / 菜单 / 路由）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:framework-agreement:%' AND deleted_at IS NULL")" \
    "框架协议权限码 0 行（页面属 M4-11）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE path LIKE '/business/erp/framework-agreement%' AND deleted_at IS NULL")" \
    "框架协议菜单 0 行（页面属 M4-11）"
ROUTE_HITS="$(grep -rn 'framework.agreement' "$BACKEND/app/src/business/erp/purchase/framework_agreement_ref.rs" | grep -c '#\[nik::' || true)"
assert_eq "0" "$ROUTE_HITS" "M4-08 新增 0 条路由（本卡只加校验与回写，不加接口）"
FRONT_HITS="$(grep -rl 'framework_agreement' "$ROOT/frontend/src" 2>/dev/null | wc -l)"
assert_eq "0" "$FRONT_HITS" "前端 0 处改动（页面属 M4-11）"

# ---------- 4) 空库全链重放 ----------
echo "== 4) 空库全链重放（mig_replay_fresh.sh，独立库）"
if [ "$SKIP_REPLAY" = "1" ]; then
    info "SKIP_REPLAY=1：跳过（本地快速迭代用；提交前必须跑全）"
else
    REPLAY_OUT="$TMP/m408_$$_replay.out"
    if DBHOST="$PG_HOST" DBPORT="$PG_PORT" DBUSER="$PG_USER" DBPASS="$PGPASSWORD" DBOWNER="$PG_USER" \
       bash "$ROOT/scripts/verify/mig_replay_fresh.sh" "m408$$" >"$REPLAY_OUT" 2>&1; then
        ok "空库按版本号升序重放全链通过（$(grep -oE '[0-9]+/[0-9]+ 条版本化迁移[^ ]*' "$REPLAY_OUT" | tail -1)）"
    else
        bad "空库重放失败（见 $REPLAY_OUT）"
        grep -E '✗ 失败|ERROR|== 结果' "$REPLAY_OUT" | head -6 | sed 's/^/        /'
    fi
fi

# ---------- 5) 回滚三段 ----------
echo "== 5) 回滚三段（数据护栏 / 成功 / 复原）"
psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
    -c "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, framework_agreement_id) VALUES ('00000000-0000-4000-8000-00000409000c'::uuid, 'PO-19990101-900452', gen_random_uuid(), DATE '1999-01-01', gen_random_uuid(), gen_random_uuid())" >/dev/null 2>&1
if psql_file "$DOWN"; then
    bad "有引用时 down 居然成功了（数据护栏失效）"
else
    if grep -q '回滚被拒绝' "$LOG"; then ok "有单据挂协议时 down 被拒（数据护栏生效）"; else
        bad "down 失败但不是因为数据护栏：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
fi
psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X \
    -c "DELETE FROM biz_purchase_order WHERE id='00000000-0000-4000-8000-00000409000c'::uuid" >/dev/null 2>&1
if psql_file "$DOWN"; then ok "清空后 down 成功"; else bad "清空后 down 失败：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
assert_eq "0" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name IN ('biz_purchase_order','biz_purchase_order_item') AND column_name LIKE 'framework_agreement%'")" \
    "down 后三个引用列已删"
assert_eq "0" "$(psql_q "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND indexname IN ('idx_biz_po_fwa','idx_biz_poi_fwa_item')")" \
    "down 后两个索引已删"
if psql_file "$MIG"; then ok "再 up 复原成功"; else bad "再 up 失败：$(tail -3 "$LOG" | tr '\n' ' ')"; fi
assert_eq "3" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND ((table_name='biz_purchase_order' AND column_name IN ('framework_agreement_id','framework_agreement_no_snapshot')) OR (table_name='biz_purchase_order_item' AND column_name='framework_agreement_item_id'))")" \
    "up 复原后三个引用列回来了"

# ---------- 6) 真库 Rust 用例（走真实建单入口） ----------
echo "== 6) 真库用例（--test-threads=1）"
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
if [ "${M408_BAD_URL:-}" = "1" ]; then
    export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/no_such_db_skip_guard"
    export SYS_DB_URL="$ERP_DB_URL"
    info "SKIP 自检模式（M408_BAD_URL=1）：连接串故意写错，期望 SKIP>0"
fi

OUT="$TMP/m408_$$_tests.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib business::erp::purchase::framework_agreement_ref -- --test-threads=1 --nocapture ) >"$OUT" 2>&1
LINE="$(grep -E '^test result: ' "$OUT" | tail -1)"
PASSED="$(printf '%s' "$LINE" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
FAILED="$(printf '%s' "$LINE" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
SKIPPED="$(grep -c 'SKIP:' "$OUT" || true)"
[ -z "${PASSED:-}" ] && PASSED=0
[ -z "${FAILED:-}" ] && FAILED=0
[ -z "${SKIPPED:-}" ] && SKIPPED=0
if [ "$FAILED" -eq 0 ] && [ "$PASSED" -ge 19 ]; then
    ok "本卡用例全绿（$PASSED 条：5 纯逻辑 + 14 真库）"
elif [ "$FAILED" -ne 0 ]; then
    bad "本卡用例有 $FAILED 条失败（见 $OUT）"
    grep -E 'panicked at|assertion|left:|right:' -A 2 "$OUT" | head -30 | sed 's/^/        /'
else
    bad "本卡用例只跑了 $PASSED 条（期望 ≥ 19，见 $OUT）"
fi
if [ "$SKIPPED" -eq 0 ]; then ok "本组 SKIP=0（真库用例没被「连不上」打成假绿）"; else
    bad "本组出现 $SKIPPED 行 SKIP（假绿）"; grep 'SKIP:' "$OUT" | head -5 | sed 's/^/        /'
fi
POS=0
for marker in '[M4-08] 可选引用' '[M4-08] 价格 超容差' '[M4-08] 价格 含税协议' '[M4-08] 数量 明细上限' '[M4-08] 数量 单头总量' '[M4-08] 金额 单头总额' '[M4-08] 改单' '[M4-08] 作废' '[M4-08] 软删' '[M4-08] 可用性' '[M4-08] 范围' '[M4-08] 详情' '[M4-08] 清场'; do
    if grep -qF "$marker" "$OUT"; then POS=$((POS+1)); else bad "缺正向标记：$marker"; fi
done
if [ "$POS" -eq 13 ]; then ok "正向标记 13/13 在位（每条用例都真跑过）"; fi

# ---------- 7) 门禁：权限审计 + 菜单结构 ----------
echo "== 7) 门禁（permission_audit_tests + menu_invariants.sh）"
AUDIT_OUT="$TMP/m408_$$_audit.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib system::menu::permission_audit_tests -- --test-threads=1 ) >"$AUDIT_OUT" 2>&1
AUDIT_LINE="$(grep -E '^test result: ' "$AUDIT_OUT" | tail -1)"
if printf '%s' "$AUDIT_LINE" | grep -qE 'test result: ok\.' && ! printf '%s' "$AUDIT_LINE" | grep -qE '[1-9][0-9]* failed'; then
    ok "permission_audit_tests 全绿（本卡没动权限码，也不许打破审计）"
else
    bad "permission_audit_tests 不是全绿：${AUDIT_LINE:-无输出}（见 $AUDIT_OUT）"
fi
if PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" >"$TMP/m408_$$_menu.out" 2>&1; then
    ok "menu_invariants.sh 8 条断言全绿"
else
    bad "menu_invariants.sh 报红（见 $TMP/m408_$$_menu.out）"
    tail -15 "$TMP/m408_$$_menu.out" | sed 's/^/        /'
fi
info "菜单总数：$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")（门禁区间 400-540；本卡零新增）"

# ---------- 8) 夹具残留 ----------
echo "== 8) 夹具残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_framework_agreement WHERE agreement_name LIKE 'M408-%'")" "M408- 协议 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'PO-19990101-9004%'")" "演练探针采购单 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE framework_agreement_no_snapshot LIKE 'FWA-19990101-94%'")" "演练探针协议号快照 0 残留"

# ---------- 9) 汇总 ----------
echo
if [ "$FAIL" -eq 0 ]; then
    printf '\033[32m== M4-08 演练结果：PASS=%s  FAIL=0 —— 框架协议订单引用（价格 / 数量校验 · 超限拒绝）就绪\033[0m\n' "$PASS"
    exit 0
else
    printf '\033[31m== M4-08 演练结果：PASS=%s  FAIL=%s —— 见上面红行\033[0m\n' "$PASS" "$FAIL"
    exit 1
fi
