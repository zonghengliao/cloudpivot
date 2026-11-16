#!/usr/bin/env bash
# ============================================================================
# M2-06 在途库存「发货 → 签收」全流程单测 · 真库演练
#
# 卡：t_b3bcd66b（M2-06【M2·仓库】在途库存：单测 / 发货→签收 全流程）
# 上游：t_7ba5b0e2（M2-01 表）/ t_6c090f97（M2-02 过账）/ t_5f324a4c（M2-03 签收）
#       t_f1bce846（M2-04 查询）/ t_b545b37a（M2-05 对账）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 补模板落后的业务迁移 V0405…V0422（V0420 建在途账 / V0421 在途读码 / V0422 对账码）
#   3. 断言在途账表与两个权限码就位（迁移没跑成功时用例会以「表不存在」的形式红，
#      先在这里一眼看出来是环境问题不是代码问题）
#   4. 真库用例（--test-threads=1）：本卡 `transit_flow_tests` 7 条全绿 + SKIP=0
#   5. 回归：上游 `shipment`（发货域写路径）与整组 `inventory_transit`
#      （M2-04 查询 + M2-05 对账）不许被本卡打破
#   6. 断言夹具清场：跑完库里没有 `M206-` 残留（别的模块有全表断言，夹具不能带出去）
#   7. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m206_transit_flow_drill.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m206_transit_flow_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m206_transit_flow_drill.sh          # 留库排查（打印库名）
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
APP_DIR="$ROOT/backend/app"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m206_flow_drill_$$"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/tmp}"
LOG="$TMP/m206_drill_$$.log"
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

printf '== M2-06 在途库存「发货→签收」全流程单测 · 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 补齐迁移 V0405…V0422 ----------
echo "== 1) 补齐业务迁移 V0405…V0422（幂等，逐条 ON_ERROR_STOP）"
MIG_FAIL=0
APPLIED=0
for n in 05 06 07 08 09 10 11 12 13 14 15 16 17 18 19 20 21 22; do
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
    ok "$APPLIED 条业务迁移全部应用成功"
fi
assert_eq "biz_inventory_in_transit" "$(psql_q "SELECT to_regclass('public.biz_inventory_in_transit')")" "在途账表存在（V0420）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = 'business:erp:inventory-transit:list' AND deleted_at IS NULL")" "在途读码就位（V0421）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code = 'business:erp:inventory-transit:audit' AND deleted_at IS NULL")" "在途对账码就位（V0422）"

# ---------- 2) 真库用例 ----------
# ⚠ ERP_DB_URL / SYS_DB_URL 必须先导出：写漏会打一排 SKIP 然后「0 失败」= 假绿，
# 所以下面同时断言 SKIP=0。
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
echo "== 2) 真库用例（--test-threads=1）"

run_group() { # 模块路径 最少用例数 期望 SKIP 数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m206_$$_${module//[:/]/_}.out"
    ( cd "$APP_DIR" && cargo test --lib "$module" -- --test-threads=1 ) >"$out" 2>&1
    line="$(grep -E '^test result: ' "$out" | head -1)"
    passed="$(printf '%s' "$line" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
    skipped="$(grep -c '^SKIP' "$out" || true)"
    [ -z "${passed:-}" ] && passed=0
    [ -z "${failed:-}" ] && failed=0
    [ -z "${skipped:-}" ] && skipped=0
    if [ "${failed:-0}" -eq 0 ]; then
        assert_ge "$min" "$passed" "$label：$module 全绿（$passed 条）"
    else
        bad "$label：$module 有 $failed 条失败（见 $out）"
    fi
    if [ "$skipped" -eq 0 ]; then
        ok "$label：本组 SKIP=0（真库用例没被「无 ERP_DB_URL」打成假绿）"
    else
        bad "$label：本组出现 $skipped 行 SKIP（真库用例被跳过 = 假绿）"
    fi
}

run_group "business::erp::inventory_transit::transit_flow_tests" 7 "本卡（7 条全流程用例）"
run_group "business::erp::inventory_transit" 48 "整组在途（本卡 7 + M2-04 查询 + M2-05 对账 + 纯逻辑）"
run_group "business::erp::shipment" 40 "上游发货域（过账 / 签收写路径未被破坏）"

# ---------- 3) 夹具清场 ----------
echo "== 3) 夹具清场（不允许把 M206- 残留带出本组）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_shipment WHERE ship_no LIKE 'M206-%'")" "biz_shipment 无 M206- 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE so_no LIKE 'M206-%'")" "biz_sales_order 无 M206- 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit t JOIN biz_material m ON m.id = t.material_id WHERE m.material_code LIKE 'M206-%'")" "在途账无 M206 夹具行"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code LIKE 'M206-%')")" "库存账无 M206 库存"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
