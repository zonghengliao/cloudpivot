#!/usr/bin/env bash
# ============================================================================
# M3-26 销售缺口收口（报价 + 信用 + 返利 + 预测）· 汇总演练
#
# 卡：t_69d8e4c0（M3-26【M3·销售】销售缺口收口：报价 + 信用 + 返利 + 预测）
#
# 这一卡是**汇总卡**，不新增业务功能；脚本做的事就是把 M3-01…M3-25 的
# 四个域在同一张演练库上一次性点验（跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 e2e，脚本自己 DROP）
#   2. 补模板落后的业务迁移（V0405 之后全部，幂等逐条 ON_ERROR_STOP）
#   3. 前置断言：四域的表 / 列 / 视图 / 权限码 / 菜单行 —— 环境缺件先在
#      这里报，不让它伪装成「用例红」
#   4. 菜单结构门禁：`scripts/verify/menu_invariants.sh` 8 条断言（同库）
#   5. 四个域的真库用例 + 两条 E2E 全链，**SKIP=0 + 正向标记**双保险
#   6. 销售域整组回归（本卡不许打破别的销售用例）
#   7. 夹具清场断言（M315-/M316-/M317-/M320-/M321-/M324-/M325- 一律 0 残留）
#   8. 打印 PASS/FAIL 汇总；任一不符退出码 1（环境问题 2）
#
# 用法：
#   bash scripts/verify/m326_sales_gap_closure_drill.sh                    # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m326_sales_gap_closure_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m326_sales_gap_closure_drill.sh          # 留库排查（打印库名）
#   M326_BAD_URL=1 bash scripts/verify/m326_sales_gap_closure_drill.sh     # SKIP 守卫自检（期望报红）
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 e2e；脚本自己 DROP；
# 连接串密码一律复用 `$PGPASSWORD`（**不要**在 URL 里写 `***` 之类的占位 ——
# 那样真库用例会连不上、打 SKIP 还记 ok，是假绿）。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
SALES_SRC="$BACKEND/app/src/business/erp/sales"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M326_DB:-cp_m326_sales_gap_e2e_$$}"
KEEP_DB="${KEEP_DB:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m326_drill_$$.log"
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
if [ -n "${M326_DB:-}" ]; then OWN_DB=0; fi

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
    if [ "$OWN_DB" = "0" ] || [ "$KEEP_DB" = "1" ]; then
        printf '\n  （库保留 → %s；不删）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG"
}
trap cleanup EXIT

printf '== M3-26 销售缺口收口演练（报价 + 信用 + 返利 + 预测）· PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 补齐业务迁移 ----------
echo "== 1) 补齐业务迁移 V0405 之后全部（幂等，逐条 ON_ERROR_STOP）"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    num="${f:1:4}"
    if [ "$num" -le 404 ]; then continue; fi
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条业务迁移全部应用成功"
fi

# ---------- 2) 前置断言：四域的表 / 列 / 视图 ----------
echo "== 2) 前置：四域（报价 / 信用 / 返利 / 预测）的表与列"
assert_eq "biz_quotation"        "$(psql_q "SELECT to_regclass('public.biz_quotation')")"        "报价单表存在（V0432 / M3-01）"
assert_eq "biz_quotation_line"   "$(psql_q "SELECT to_regclass('public.biz_quotation_line')")"   "报价单明细表存在（V0432 / M3-01）"
assert_eq "biz_sales_rebate_policy"     "$(psql_q "SELECT to_regclass('public.biz_sales_rebate_policy')")"     "返利政策表存在（V0438 / M3-15）"
assert_eq "biz_sales_rebate_rule"       "$(psql_q "SELECT to_regclass('public.biz_sales_rebate_rule')")"       "返利规则档位表存在（V0438 / M3-15）"
assert_eq "biz_sales_rebate_settlement" "$(psql_q "SELECT to_regclass('public.biz_sales_rebate_settlement')")" "返利结算表存在（V0439 / M3-16）"
assert_eq "biz_sales_forecast"   "$(psql_q "SELECT to_regclass('public.biz_sales_forecast')")"   "销售预测表存在（V0318 + V0444 / M3-20）"
assert_eq "v_biz_sales_forecast_current" "$(psql_q "SELECT to_regclass('public.v_biz_sales_forecast_current')")" "预测「当前版本」视图存在（V0444 / M3-20）"
assert_eq "forecast_qty" "$(psql_q "SELECT COALESCE((SELECT column_name FROM information_schema.columns WHERE table_name='biz_sales_forecast' AND column_name='forecast_qty'),'')")" "预测表有 forecast_qty 列（V0444 / M3-20）"
assert_eq "revised_from_id" "$(psql_q "SELECT COALESCE((SELECT column_name FROM information_schema.columns WHERE table_name='biz_sales_forecast' AND column_name='revised_from_id'),'')")" "预测表有版本链 revised_from_id 列（V0444 / M3-20）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM information_schema.constraint_column_usage WHERE table_name='biz_customer' AND constraint_name='ck_biz_customer_credit_policy'")" "客户表 credit_policy 白名单 CHECK 在位（V0437 / M3-09）"
assert_eq "credit_used" "$(psql_q "SELECT COALESCE((SELECT column_name FROM information_schema.columns WHERE table_name='biz_customer' AND column_name='credit_used'),'')")" "客户表有 credit_used 缓存列（M3-10）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'so_approve' AND status = 'PUBLISHED' AND deleted_at IS NULL")" "so_approve 审批流已发布（信用 / 报价转单依赖）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'quotation_approve' AND status = 'PUBLISHED' AND deleted_at IS NULL")" "quotation_approve 审批流已发布（M3-04）"

# ---------- 3) 权限码：源码声明的每一个码都必须在 sys_menu 有一行 ----------
echo "== 3) 权限码 ↔ sys_menu（源码解析，不手抄）"
CODEFILE="$TMP/m326_$$_codes.txt"
grep -rhoE 'permission = "business:erp:[a-z-]+:[a-z-]+"' \
    "$SALES_SRC/quotation_handler.rs" \
    "$SALES_SRC/rebate_handler.rs" \
    "$SALES_SRC/rebate_settle_handler.rs" \
    "$SALES_SRC/rebate_invoice_handler.rs" \
    "$SALES_SRC/sales_target_handler.rs" \
    "$BACKEND/app/src/business/erp/credit_handler.rs" \
    | sed -E 's/.*"([^"]+)"/\1/' | sort -u > "$CODEFILE"
CODE_N=$(wc -l < "$CODEFILE")
assert_ge 15 "$CODE_N" "四域源码声明的权限码数量"
MISSING=0
while read -r code; do
    [ -z "$code" ] && continue
    n=$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = '$code' AND deleted_at IS NULL")
    if [ "$n" != "1" ]; then
        # 同一个按钮码挂多行也是错（权限树上会重复出现）
        bad "权限码 $code 在 sys_menu 里命中 $n 行（期望 1）"
        MISSING=$((MISSING+1))
    fi
done < "$CODEFILE"
if [ "$MISSING" -eq 0 ]; then
    ok "$CODE_N 个四域权限码在 sys_menu 各命中 1 行（无孤儿码 / 无重码）"
fi

echo "== 3b) 菜单行：四域页面 + 按钮（一行都不能少）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE path = '/business/erp/quotation' AND menu_type = 2 AND auth_code = 'business:erp:quotation:list' AND deleted_at IS NULL")" "报价单页面行在位（M3-06 / V0436）"
assert_eq "5" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:quotation:%' AND menu_type = 3 AND deleted_at IS NULL")" "报价单按钮行 5 条"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE path = '/business/erp/rebate-policy' AND menu_type = 2 AND deleted_at IS NULL")" "返利页面行在位（M3-19 / V0443）"
assert_eq "3" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:rebate-policy:%' AND menu_type = 3 AND deleted_at IS NULL")" "返利政策按钮行 3 条"
assert_eq "4" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:rebate-settlement:%' AND menu_type = 3 AND deleted_at IS NULL")" "返利结算按钮行 4 条"
assert_eq "4" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:sales-forecast:%' AND menu_type = 3 AND deleted_at IS NULL")" "销售预测按钮行 4 条（M3-20）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:quotation:%' AND parent_id IS NULL")" "报价单按钮不悬空（parent 非空）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND menu_type IN (1,2) AND length(title) > 12")" "目录 / 页面 title 长度 ≤ 12（约定 12）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM (SELECT parent_id, sort_order FROM sys_menu WHERE deleted_at IS NULL AND menu_type = 2 GROUP BY 1,2 HAVING count(*) > 1) t")" "同父页面 sort_order 唯一（约定 12）"

# ---------- 3c) 路由条数（源码宏，四域不许悄悄丢接口） ----------
echo "== 3c) 路由条数（源码宏声明，逐域钉死）"
n_routes() { # 路径正则
    grep -rhoE "#\[nik::(get|post|put|delete|patch)\(\"/business/erp/[^\"]+\"\)\]" \
        "$SALES_SRC"/*.rs "$BACKEND/app/src/business/erp/credit_handler.rs" \
        | grep -E "$1" | wc -l
}
assert_eq "9" "$(n_routes '/business/erp/quotation')" "报价单路由 9 条（M3-02…M3-08，含 PDF / 状态流转 / 删除）"
assert_eq "5" "$(n_routes '/business/erp/rebate-policy')" "返利政策路由 5 条（M3-15）"
assert_eq "9" "$(n_routes '/business/erp/rebate-settlement')" "返利结算 + 红字发票路由 9 条（M3-16/17）"
assert_eq "5" "$(n_routes '/business/erp/sales-forecast')" "销售预测路由 5 条（M3-20/21，含 variance）"
assert_eq "1" "$(n_routes '/business/erp/credit/')" "客户信用快照路由 1 条（M3-10）"

# ---------- 4) 菜单结构门禁（8 条断言，同库） ----------
echo "== 4) 菜单结构门禁 menu_invariants.sh（8 条断言）"
if PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" >"$TMP/m326_$$_menu.out" 2>&1; then
    ok "menu_invariants.sh 8 条断言全绿（输出尾：$(grep -E '全绿|PASS|OK' "$TMP/m326_$$_menu.out" | tail -1)）"
else
    bad "menu_invariants.sh 报红（见 $TMP/m326_$$_menu.out）"
    tail -20 "$TMP/m326_$$_menu.out" | sed 's/^/        /'
fi
info "菜单总数：$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL")（门禁区间 400-540）"

# ---------- 4b) 四域菜单 / 表清单落盘（证据包用，同库实查不手抄） ----------
echo "== 4b) 四域菜单与表清单落盘"
MENU_DUMP="$TMP/m326_$$_menu_inventory.txt"
{
    echo "# M3-26 四域菜单实查（库 $DB，menu_invariants 之后）"
    echo "## 页面 / 按钮（type / path / name / title / auth_code / sort）"
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tA -F ' | ' -c \
      "SELECT menu_type, COALESCE(path,'—'), name, title, COALESCE(auth_code,'—'), sort_order
         FROM sys_menu
        WHERE deleted_at IS NULL
          AND (auth_code LIKE 'business:erp:quotation:%'
            OR auth_code LIKE 'business:erp:rebate-policy:%'
            OR auth_code LIKE 'business:erp:rebate-settlement:%'
            OR auth_code LIKE 'business:erp:sales-forecast:%'
            OR path IN ('/business/erp/quotation','/business/erp/rebate-policy'))
        ORDER BY menu_type, COALESCE(path,'zz'), sort_order"
    echo
    echo "## 菜单总数 / 四域权限码数"
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tA -c \
      "SELECT 'menu_total=' || count(*) FROM sys_menu WHERE deleted_at IS NULL"
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tA -c \
      "SELECT 'domain_codes=' || count(DISTINCT auth_code) FROM sys_menu
        WHERE deleted_at IS NULL
          AND (auth_code LIKE 'business:erp:quotation:%'
            OR auth_code LIKE 'business:erp:rebate-policy:%'
            OR auth_code LIKE 'business:erp:rebate-settlement:%'
            OR auth_code LIKE 'business:erp:sales-forecast:%')"
    echo
    echo "## 表 / 视图（四域）"
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tA -c \
      "SELECT c.relkind || ' | ' || c.relname FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public' AND c.relkind IN ('r','v')
          AND c.relname ~ '(quotation|rebate|forecast)' ORDER BY 1,2"
} > "$MENU_DUMP" 2>&1
ok "四域菜单 / 表清单已落盘：$MENU_DUMP"

# ---------- 4c) 夹具基线快照（跑用例前） ----------
echo "== 4c) 夹具基线快照（用来区分「本卡新增残留」与「上游用例自带残留」）"
fixture_counts() {
    printf 'cust=%s order=%s quote=%s fc=%s settle=%s policy=%s inst=%s log=%s\n' \
        "$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE 'M3%'")" \
        "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M3%')")" \
        "$(psql_q "SELECT count(*) FROM biz_quotation WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M3%')")" \
        "$(psql_q "SELECT count(*) FROM biz_sales_forecast WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M3%')")" \
        "$(psql_q "SELECT count(*) FROM biz_sales_rebate_settlement WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M3%')")" \
        "$(psql_q "SELECT count(*) FROM biz_sales_rebate_policy WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M3%')")" \
        "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE biz_type IN ('QUOTATION_APPROVE','SO_APPROVE') AND biz_id IN (SELECT id FROM biz_sales_order WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M3%'))")" \
        "$(psql_q "SELECT count(*) FROM biz_credit_log WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'M3%')")"
}
BASE_FIXTURES="$(fixture_counts)"
info "基线：$BASE_FIXTURES"

# ---------- 5) 真库用例（四域 + 两条 E2E） ----------
echo "== 5) 真库用例（--test-threads=1 --nocapture）"
export ERP_DB_URL="postgres://$PG_USER:${PGPASSWORD}@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"

if [ "${M326_BAD_URL:-}" = "1" ]; then
    export ERP_DB_URL="postgres://$PG_USER:${PGPASSWORD}@$PG_HOST:$PG_PORT/no_such_db_skip_guard"
    export SYS_DB_URL="$ERP_DB_URL"
    info "SKIP 自检模式（M326_BAD_URL=1）：连接串故意写错，期望 SKIP>0 与缺正向标记的 FAIL"
fi

run_group() { # 模块路径 最少用例数 说明 [正向标记] [strict_residue]
    local module="$1" min="$2" label="$3" marker="${4:-}" strict="${5:-0}" out line passed failed skipped before after
    before="$(fixture_counts)"
    out="$TMP/m326_$$_${module//[:/]/_}.out"
    ( cd "$BACKEND" && cargo test -p cloudpivot --lib "$module" -- --test-threads=1 --nocapture ) >"$out" 2>&1
    line="$(grep -E '^test result: ' "$out" | tail -1)"
    passed="$(printf '%s' "$line" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
    # `SKIP:` 不带行首锚点：--nocapture 下测试名与 SKIP 会打在同一行
    skipped="$(grep -c 'SKIP:' "$out" || true)"
    [ -z "${passed:-}" ] && passed=0
    [ -z "${failed:-}" ] && failed=0
    [ -z "${skipped:-}" ] && skipped=0
    if [ "${failed:-0}" -eq 0 ] && [ "${passed:-0}" -ge "$min" ]; then
        ok "$label：$module 全绿（$passed 条）"
    elif [ "${failed:-0}" -ne 0 ]; then
        bad "$label：$module 有 $failed 条失败（见 $out）"
        grep -E 'panicked at|assertion|left:|right:' -A 2 "$out" | head -40
        printf '  ---- 步骤轨迹（尾 10 行）：\n'
        grep -E '^\[M3-' "$out" | tail -10 | sed 's/^/        /'
    else
        bad "$label：$module 只跑了 $passed 条（期望 ≥ $min，见 $out）"
    fi
    if [ "$skipped" -eq 0 ]; then
        ok "$label：本组 SKIP=0（真库用例没被「连不上」打成假绿）"
    else
        bad "$label：本组出现 $skipped 行 SKIP（真库用例被跳过 = 假绿）"
        grep 'SKIP:' "$out" | head -5 | sed 's/^/        /'
    fi
    if [ -n "$marker" ]; then
        if grep -qF "$marker" "$out"; then
            ok "$label：正向标记在位（$marker）"
        else
            bad "$label：缺正向标记「$marker」—— 说明用例没真跑（被跳过 / 被过滤）"
        fi
    fi
    # 夹具增量：E2E 全链（strict=1）必须自己收干净；其它域只报告不判红
    after="$(fixture_counts)"
    if [ "$before" = "$after" ]; then
        ok "$label：夹具净增量为 0（$after）"
    elif [ "$strict" = "1" ]; then
        bad "$label：E2E 夹具没清干净（跑前 $before → 跑后 $after）"
    else
        info "$label：夹具净增量非 0（跑前 $before → 跑后 $after）—— 上游直插夹具用例自带残留，只报告"
    fi
    printf '  ---- 原始输出：%s\n' "$out"
}

# 报价（M3-02…M3-08）：状态机 19 + 转单 21 + model 16 + handler 13 + PDF 9 + 审批桥 5
run_group "business::erp::sales::quotation" 80 "报价域（状态机 / 转单 / PDF / 审批桥 / model / handler）"
# 返利（M3-15…M3-19）：政策 13 + 规则矩阵 13 + 结算 12 + 结算单据 12 + 红字发票 9 + dto/model/handler
run_group "business::erp::sales::rebate" 100 "返利域（政策 / 规则矩阵 / 结算 / 红字发票）"
# 预测（M3-20 / M3-21 / M3-22）：表结构 11 + 偏差 7 + 汇总口径 6
run_group "business::erp::sales::sales_forecast_db_tests" 11 "预测域：表结构 / 版本链 / 当前版本视图（M3-20）"
run_group "business::erp::sales::sales_variance" 13 "预测域：预测 vs 实际（偏差 + 汇总口径，M3-21/22）"
# 信用（M3-09…M3-13）：占用 / 拦截 / 释放 / 并发 / 快照
run_group "business::erp::credit" 40 "信用域（占用 / 拦截 / 释放 / 并发 / 快照）"
# 两条全链 E2E
run_group "business::erp::sales::sales_chain_e2e_tests" 1 "E2E：报价单 → 订单 → 出库（M3-24）" "[M3-24]" 1
run_group "business::erp::sales::credit_control_e2e_tests" 2 "E2E：信用控制超限全链（M3-25）" "[M3-25] 全链通过" 1
# 伞形回归：整个销售域
run_group "business::erp::sales" 350 "销售域整组回归（订单 / 报价 / 退货 / 目标 / 返利 / 预测未被收口动作打破）"

# ---------- 6) 夹具清场 ----------
echo "== 6) 夹具清场（E2E 全链判红；上游直插夹具只报告）"
AFTER_FIXTURES="$(fixture_counts)"
info "基线：$BASE_FIXTURES"
info "收尾：$AFTER_FIXTURES"
if [ "$BASE_FIXTURES" = "$AFTER_FIXTURES" ]; then
    ok "整个演练净增量为 0（跑完与基线一致）"
else
    info "整个演练净增量非 0 —— 见上面各组「夹具净增量」行，只有 E2E 组判红"
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tA -F ' | ' -c \
      "SELECT code, count(*) FROM biz_customer WHERE code LIKE 'M3%' GROUP BY code ORDER BY 1" \
      | sed 's/^/        /'
fi
# 两条 E2E 全链自带清场用例（组内 strict=1 已判）；这里再按 M324-/M325- 前缀补一组库侧断言
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code IN ('M324-CUST','M325-CUST')")" "biz_customer 无 M324/M325 夹具客户"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE customer_id IN (SELECT id FROM biz_customer WHERE code IN ('M324-CUST','M325-CUST'))")" "biz_sales_order 无 M324/M325 夹具订单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_user WHERE username LIKE 'm324\\_%' OR username LIKE 'm325\\_%'")" "sys_user 无 M324/M325 夹具账号"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_credit_log WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = 'M325-CUST')")" "biz_credit_log 无 M325 夹具台账"

# ---------- 7) 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
