#!/usr/bin/env bash
# ============================================================================
# M4-01 采购计划表结构（年度 / 月度，物料 × 仓库）· 演练
#
# 卡：t_c5ecacf7（M4-01【M4·采购】采购计划：表结构（年度/月度，物料 × 仓库））
#
# 这一卡**只交表结构**，所以脚本验的也是结构本身（不是业务链路）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 应用 V0446 —— **连跑两遍**证明幂等（第二遍只该出 NOTICE，不许 ERROR）
#   3. 结构断言（psql 侧，独立于 Rust）：表 / 列数 / 索引 / 外键级联 / 取号行
#   4. 约束行为断言：6 类坏数据必须被库层拒（年度带月、月度无月、月 13、
#      死状态 CLOSED、SUBMITTED 无实例、同计划同物料同仓重复）
#   5. 本卡**不许夹带**：采购计划的权限码 0 个、菜单行 0 行
#   6. 空库全链重放：`scripts/verify/mig_replay_fresh.sh` 把 360+ 条迁移按版本号
#      在空库上重跑一遍（V0446 不许依赖更晚的迁移 / 不许和别人的号撞车）
#   7. 回滚三段：有数据 → 拒；清空 → 成功；再 up → 复原
#   8. Rust 用例：本卡 15 条（4 纯逻辑 + 11 真库），**SKIP=0 + 正向标记**双保险
#   9. 门禁：permission_audit_tests 8/8 + menu_invariants.sh 8 条（本卡零菜单，跑一遍
#      证明没顶破 540 上限）
#  10. 夹具残留：`M401-` 计划与明细 0 条
#
# 用法：
#   bash scripts/verify/m401_purchase_plan_schema_drill.sh                    # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m401_purchase_plan_schema_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m401_purchase_plan_schema_drill.sh          # 留库排查（打印库名）
#   M401_BAD_URL=1 bash scripts/verify/m401_purchase_plan_schema_drill.sh     # SKIP 守卫自检（期望报红）
#   SKIP_REPLAY=1 bash scripts/verify/m401_purchase_plan_schema_drill.sh      # 跳过空库全链重放（省 2 分钟）
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 连接串密码一律复用 `$PGPASSWORD`（**不要**在 URL 里写占位符 —— 那样真库用例会
# 连不上、打 SKIP 还记 ok，是假绿）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
MIG="$MIG_DIR/V0446__purchase_plan.up.sql"
DOWN="$MIG_DIR/V0446__purchase_plan.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M401_DB:-cp_m401_schema_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_REPLAY="${SKIP_REPLAY:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m401_drill_$$.log"
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
if [ -n "${M401_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }
psql_sql()   { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f - >"$LOG" 2>&1; }

assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

# 期望这条 SQL 被库层拒（psql 退出码非 0 且输出里有 ERROR）
rejects() { # sql 说明
    local out
    out="$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -c "$1" 2>&1)"
    if printf '%s' "$out" | grep -q 'ERROR'; then
        ok "$2（库层拒了：$(printf '%s' "$out" | grep -oE 'constraint "[a-z_]+"|violates [a-z ]+constraint "[a-z_]+"' | head -1)）"
    else
        bad "$2 —— 库层居然接受了（约束失效）"
    fi
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

printf '== M4-01 采购计划表结构演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 把演练库推到仓库头部（V0405…V0445），再上 V0446 ----------
# 模板库 `cloudpivot_test` 停在 V0404 左右（501 条菜单），而仓库头部是 V0446 / 540 条菜单。
# **直接只上 V0446 是不行的**：permission_audit 的 n18（ROUTE_LEVEL_ONLY 台账）会拿
# 「台账里的码 vs 菜单里的码」对拍，模板落后就会误报台账过期（实测：
# inventory-transit:page / report:receivable-aging）。所以先按版本号把 V0405+ 补齐，
# 让门禁跑在**和仓库同代**的库上 —— 这是 m326 汇总演练同款做法。
echo "== 1) 补齐业务迁移 V0405 之后（除 V0446）+ 再上 V0446"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    num="${f:1:4}"
    if [ "$num" -le 404 ]; then continue; fi
    if [ "$num" -ge 446 ]; then continue; fi   # V0446 单独上（要单测幂等）
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条 V0405…V0445 迁移全部应用成功（演练库推到 V0445）"
fi
MENU_BEFORE="$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
assert_eq "GONE" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_purchase_plan')::text,'GONE')")" "上 V0446 之前计划表还不存在（下面每一条断言才说明是 V0446 建的）"

echo "== 1b) 应用 V0446（连跑两遍验幂等）"
if psql_file "$MIG"; then ok "第一遍应用成功"; else
    bad "第一遍应用失败：$(tail -3 "$LOG" | tr '\n' ' ')"; exit 2
fi
if psql_file "$MIG"; then ok "第二遍应用成功（幂等：只该出 NOTICE）"; else
    bad "第二遍应用失败（幂等破了）：$(tail -3 "$LOG" | tr '\n' ' ')"
fi
if grep -q 'ERROR' "$LOG"; then
    bad "第二遍输出里出现 ERROR：$(grep 'ERROR' "$LOG" | head -2 | tr '\n' ' ')"
else
    ok "第二遍输出无 ERROR（CREATE ... IF NOT EXISTS 生效）"
fi
MENU_AFTER="$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")"
assert_eq "$MENU_BEFORE" "$MENU_AFTER" "V0446 一条菜单都没加（上迁移前后菜单总数不变；本卡只做表结构）"

# ---------- 2) 结构断言（psql 侧，独立于 Rust） ----------
echo "== 2) 结构断言"
assert_eq "biz_purchase_plan"      "$(psql_q "SELECT COALESCE(to_regclass('public.biz_purchase_plan')::text,'')")"      "计划单头表存在"
assert_eq "biz_purchase_plan_item" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_purchase_plan_item')::text,'')")" "计划明细表存在"
assert_eq "22" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_purchase_plan'")" "单头 22 列"
assert_eq "13" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_purchase_plan_item'")" "明细 13 列"
assert_eq "CASCADE" "$(psql_q "SELECT delete_rule FROM information_schema.referential_constraints WHERE constraint_name='fk_biz_ppi_plan'")" "明细外键 ON DELETE CASCADE"
assert_eq "8" "$(psql_q "SELECT count(DISTINCT indexname) FROM pg_indexes WHERE schemaname='public' AND indexname IN ('uk_biz_purchase_plan_no','uk_biz_purchase_plan_period','idx_biz_purchase_plan_period','idx_biz_purchase_plan_status','idx_biz_purchase_plan_parent','uk_biz_pp_item_cell','uk_biz_pp_item_line','idx_biz_pp_item_material')")" "8 个索引全在"
assert_eq "2" "$(psql_q "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND indexname IN ('uk_biz_purchase_plan_no','uk_biz_purchase_plan_period') AND indexdef LIKE '%WHERE (deleted_at IS NULL)%'")" "两个单头唯一索引都是部分索引（软删后可重建）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='PPL' AND deleted_at IS NULL AND prefix='PPL' AND separator='-'")" "取号规则 PPL 一行"
assert_eq "1" "$(psql_q "SELECT (count(*) > 180)::int FROM information_schema.tables WHERE table_schema='public'")" "模板库克隆完整（public 表 > 180 张）"

# ---------- 3) 约束行为：坏数据必须被拒 ----------
echo "== 3) 约束行为（坏数据逐条必须被库层拒）"
rejects "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month) VALUES ('PPL-19990101-900901','年度带月','ANNUAL',2026,3)" "年度计划带月份被拒（ck_biz_pp_period）"
rejects "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month) VALUES ('PPL-19990101-900902','月度无月','MONTHLY',2026,NULL)" "月度计划没有月份被拒（3VL 陷阱已修：CHECK 里 plan_month IS NOT NULL）"
rejects "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month) VALUES ('PPL-19990101-900903','月度十三月','MONTHLY',2026,13)" "月度 13 月被拒"
rejects "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, status) VALUES ('PPL-19990101-900904','死状态','ANNUAL',2026,'CLOSED')" "状态域外的 CLOSED 被拒（不造死状态）"
rejects "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, status) VALUES ('PPL-19990101-900905','无实例','ANNUAL',2026,'SUBMITTED')" "SUBMITTED 没有审批实例被拒"
rejects "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year) VALUES ('M401-BAD-NO','坏单号','ANNUAL',2026)" "单号形状不合 PPL-yyyyMMdd-000001 被拒"

# 正样本：一张月度计划 + 两行明细（同物料换仓），再验粒度唯一 + 计划量 > 0 + 执行量可超计划
psql_sql <<'SQL'
INSERT INTO biz_purchase_plan (id, plan_no, plan_name, plan_type, plan_year, plan_month)
VALUES ('00000000-0000-4000-8000-000000401001'::uuid, 'PPL-19990101-900950', 'M401-演练月度计划', 'MONTHLY', 2026, 11);
INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty)
SELECT '00000000-0000-4000-8000-000000401001'::uuid, 1, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 100;
INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty)
SELECT plan_id, 2, material_id, gen_random_uuid(), unit_id, 50 FROM biz_purchase_plan_item
 WHERE plan_id = '00000000-0000-4000-8000-000000401001'::uuid AND line_no = 1;
SQL
if [ $? -eq 0 ]; then ok "正样本：月度计划 + 同物料换仓两行都能插"; else bad "正样本插不进去：$(tail -2 "$LOG" | tr '\n' ' ')"; fi

rejects "INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty) SELECT plan_id, 3, material_id, warehouse_id, unit_id, 10 FROM biz_purchase_plan_item WHERE plan_id='00000000-0000-4000-8000-000000401001'::uuid AND line_no=1" "同计划「同物料 + 同仓库」第二行被拒（卡面粒度 uk_biz_pp_item_cell）"
rejects "INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty) VALUES ('00000000-0000-4000-8000-000000401001'::uuid, 4, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 0)" "计划量 0 被拒（ck_biz_ppi_qty）"
assert_eq "true" "$(psql_q "SELECT (total_planned_qty = 0 AND total_executed_qty = 0 AND total_planned_amount = 0)::text FROM biz_purchase_plan WHERE id='00000000-0000-4000-8000-000000401001'::uuid")" "单头三个合计默认 0（派生列，不许客户端传）"
accepts "UPDATE biz_purchase_plan_item SET executed_qty = 150 WHERE plan_id='00000000-0000-4000-8000-000000401001'::uuid AND line_no=1" "已执行量超计划必须写得进去（执行率 >100% 是报表的事，不在库层拦）"
rejects "UPDATE biz_purchase_plan_item SET executed_qty = -1 WHERE plan_id='00000000-0000-4000-8000-000000401001'::uuid AND line_no=1" "负已执行量被拒（ck_biz_ppi_exec）"

# ---------- 4) 本卡不许夹带：权限码 0 / 菜单 0 ----------
echo "== 4) 本卡范围守卫（不夹带权限码与菜单）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:purchase-plan:%' AND deleted_at IS NULL")" "采购计划权限码 0 行（M4-02 才落）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE path LIKE '/business/erp/purchase-plan%' AND deleted_at IS NULL")" "采购计划菜单 0 行（M4-06 才落）"
SRC_HITS="$(grep -rl "purchase-plan:" "$BACKEND/app/src/business/erp/purchase" 2>/dev/null | wc -l)"
assert_eq "0" "$SRC_HITS" "源码里 0 个 purchase-plan 权限码（连常量都不定义）"

# ---------- 5) 空库全链重放（V0446 在版本升序链里也要过） ----------
echo "== 5) 空库全链重放（mig_replay_fresh.sh，独立库）"
if [ "$SKIP_REPLAY" = "1" ]; then
    info "SKIP_REPLAY=1：跳过（本地快速迭代用；提交前必须跑全）"
else
    REPLAY_OUT="$TMP/m401_$$_replay.out"
    if DBHOST="$PG_HOST" DBPORT="$PG_PORT" DBUSER="$PG_USER" DBPASS="$PGPASSWORD" DBOWNER="$PG_USER" \
       bash "$ROOT/scripts/verify/mig_replay_fresh.sh" "m401$$" >"$REPLAY_OUT" 2>&1; then
        ok "空库按版本号升序重放全链通过（$(grep -oE '[0-9]+/[0-9]+ 条版本化迁移[^ ]*' "$REPLAY_OUT" | tail -1)）"
    else
        bad "空库重放失败（见 $REPLAY_OUT）"
        grep -E '✗ 失败|ERROR|== 结果' "$REPLAY_OUT" | head -6 | sed 's/^/        /'
    fi
fi

# ---------- 6) 回滚三段：有数据拒 / 清空成功 / 再 up 复原 ----------
echo "== 6) 回滚三段（数据护栏 / 成功 / 复原）"
if psql_file "$DOWN"; then
    bad "有数据时 down 居然成功了（数据护栏失效）"
else
    if grep -q '回滚被拒绝' "$LOG"; then ok "有数据时 down 被拒（数据护栏生效）"; else bad "down 失败但不是因为数据护栏：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
fi
psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -c "DELETE FROM biz_purchase_plan WHERE plan_name LIKE 'M401-%'" >/dev/null 2>&1
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_plan_item WHERE plan_id='00000000-0000-4000-8000-000000401001'::uuid")" "删单头连带删明细（ON DELETE CASCADE）"
if psql_file "$DOWN"; then ok "清空后 down 成功"; else bad "清空后 down 失败：$(tail -2 "$LOG" | tr '\n' ' ')"; fi
assert_eq "" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_purchase_plan')::text,'')")" "down 后两张表已删"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='PPL'")" "down 后取号行已删"
if psql_file "$MIG"; then ok "再 up 复原成功"; else bad "再 up 失败：$(tail -3 "$LOG" | tr '\n' ' ')"; fi
assert_eq "biz_purchase_plan" "$(psql_q "SELECT COALESCE(to_regclass('public.biz_purchase_plan')::text,'')")" "up 复原后表回来了"

# ---------- 7) Rust 用例（本卡 15 条；SKIP=0 + 正向标记） ----------
echo "== 7) 真库用例（--test-threads=1 --nocapture）"
export ERP_DB_URL="postgres://$PG_USER:${PGPASSWORD}@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
if [ "${M401_BAD_URL:-}" = "1" ]; then
    export ERP_DB_URL="postgres://$PG_USER:${PGPASSWORD}@$PG_HOST:$PG_PORT/no_such_db_skip_guard"
    export SYS_DB_URL="$ERP_DB_URL"
    info "SKIP 自检模式（M401_BAD_URL=1）：连接串故意写错，期望 SKIP>0 与缺正向标记的 FAIL"
fi

OUT="$TMP/m401_$$_tests.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib business::erp::purchase::purchase_plan -- --test-threads=1 --nocapture ) >"$OUT" 2>&1
LINE="$(grep -E '^test result: ' "$OUT" | tail -1)"
PASSED="$(printf '%s' "$LINE" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
FAILED="$(printf '%s' "$LINE" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
SKIPPED="$(grep -c 'SKIP:' "$OUT" || true)"
[ -z "${PASSED:-}" ] && PASSED=0
[ -z "${FAILED:-}" ] && FAILED=0
[ -z "${SKIPPED:-}" ] && SKIPPED=0
if [ "$FAILED" -eq 0 ] && [ "$PASSED" -ge 15 ]; then
    ok "本卡用例全绿（$PASSED 条：4 纯逻辑 + 11 真库）"
elif [ "$FAILED" -ne 0 ]; then
    bad "本卡用例有 $FAILED 条失败（见 $OUT）"
    grep -E 'panicked at|assertion|left:|right:' -A 2 "$OUT" | head -30 | sed 's/^/        /'
else
    bad "本卡用例只跑了 $PASSED 条（期望 ≥ 15，见 $OUT）"
fi
if [ "$SKIPPED" -eq 0 ]; then ok "本组 SKIP=0（真库用例没被「连不上」打成假绿）"; else
    bad "本组出现 $SKIPPED 行 SKIP（假绿）"; grep 'SKIP:' "$OUT" | head -5 | sed 's/^/        /'
fi
if grep -qF '[M4-01] 粒度：物料 × 仓库' "$OUT" && grep -qF '[M4-01] 表形状' "$OUT"; then
    ok "正向标记在位（[M4-01] 表形状 / 粒度）"
else
    bad "缺正向标记 —— 用例没真跑（被跳过 / 被过滤）"
fi

# ---------- 8) 门禁：权限审计 + 菜单结构 ----------
echo "== 8) 门禁（permission_audit_tests + menu_invariants.sh）"
AUDIT_OUT="$TMP/m401_$$_audit.out"
( cd "$BACKEND" && cargo test -p cloudpivot --lib system::menu::permission_audit_tests -- --test-threads=1 ) >"$AUDIT_OUT" 2>&1
AUDIT_LINE="$(grep -E '^test result: ' "$AUDIT_OUT" | tail -1)"
if printf '%s' "$AUDIT_LINE" | grep -qE 'test result: ok\.' && ! printf '%s' "$AUDIT_LINE" | grep -qE '[1-9][0-9]* failed'; then
    ok "permission_audit_tests 8/8 通过（本卡没动权限码，也不许打破审计）"
else
    bad "permission_audit_tests 不是 8/8：${AUDIT_LINE:-无输出}（见 $AUDIT_OUT）"
fi
if PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" >"$TMP/m401_$$_menu.out" 2>&1; then
    ok "menu_invariants.sh 8 条断言全绿"
else
    bad "menu_invariants.sh 报红（见 $TMP/m401_$$_menu.out）"
    tail -15 "$TMP/m401_$$_menu.out" | sed 's/^/        /'
fi
info "菜单总数：$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")（门禁区间 400-540；本卡零新增）"

# ---------- 9) 夹具残留 ----------
echo "== 9) 夹具残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_plan WHERE plan_name LIKE 'M401-%'")" "M401- 计划 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_plan_item it JOIN biz_purchase_plan p ON p.id=it.plan_id WHERE p.plan_name LIKE 'M401-%'")" "M401- 明细 0 残留"

# ---------- 10) 汇总 ----------
echo
if [ "$FAIL" -eq 0 ]; then
    printf '\033[32m== M4-01 演练结果：PASS=\033[0m%s\033[32m  FAIL=0 —— 采购计划表结构（年度/月度 · 物料 × 仓库 · 计划量/已执行量）就绪\033[0m\n' "$PASS"
    exit 0
else
    printf '\033[31m== M4-01 演练结果：PASS=%s  FAIL=%s —— 见上面红行\033[0m\n' "$PASS" "$FAIL"
    exit 1
fi
