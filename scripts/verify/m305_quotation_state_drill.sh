#!/usr/bin/env bash
# ============================================================================
# M3-05 报价单「状态机 + 转单」单测 · 真库演练
#
# 卡：t_a2ba25ed（M3-05【M3·销售】报价单：单测 / 含转单与状态机）
# 上游：t_6a61726c（M3-01 表）/ t_0750ad31（M3-02 CRUD+状态机）
#       t_de446ead（M3-03 转销售订单）/ t_6bede75a（M3-04 审批流接线）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 补模板落后的业务迁移 V0405…V0435（V0432 表 / V0433+V0434 权限码 / V0435 审批流程）
#   3. 断言表与权限码、流程种子就位（迁移没跑成功时用例会以「表不存在」红，
#      先在这里一眼看出是环境问题不是代码问题）
#   4. 真库用例（--test-threads=1）：本卡 `quotation_state_tests` 19 条全绿 + SKIP=0
#   5. 回归：报价单整组（CRUD / 状态机 / 转单 / 审批桥）+ 销售域整组不许被本卡打破
#   6. 断言夹具清场：跑完库里没有 `M305-CUST` 残留、没有销售链孤儿状态日志
#   7. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m305_quotation_state_drill.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m305_quotation_state_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m305_quotation_state_drill.sh          # 留库排查（打印库名）
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
DB="cp_m305_state_drill_$$"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/tmp}"
LOG="$TMP/m305_drill_$$.log"
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

printf '== M3-05 报价单「状态机 + 转单」单测 · 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 补齐迁移 V0405…V0435 ----------
echo "== 1) 补齐业务迁移 V0405…V0435（幂等，逐条 ON_ERROR_STOP）"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V04(0[5-9]|[1-3][0-9])__.*\.up\.sql$' | sort); do
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条业务迁移全部应用成功"
fi
assert_eq "biz_quotation" "$(psql_q "SELECT to_regclass('public.biz_quotation')")" "报价单头表存在（V0432）"
assert_eq "biz_quotation_line" "$(psql_q "SELECT to_regclass('public.biz_quotation_line')")" "报价单明细表存在（V0432）"
assert_eq "6" "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code LIKE 'business:erp:quotation:%' AND deleted_at IS NULL")" "报价单 6 个权限码就位（V0433 + V0434）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'quotation_approve' AND deleted_at IS NULL")" "quotation_approve 流程种子就位（V0435）"

# ---------- 2) 真库用例 ----------
# ⚠ ERP_DB_URL / SYS_DB_URL 必须先导出：写漏会打一排 SKIP 然后「0 失败」= 假绿，
# 所以下面同时断言 SKIP=0。
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
echo "== 2) 真库用例（--test-threads=1）"

run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m305_$$_${module//[:/]/_}.out"
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

run_group "business::erp::sales::quotation_handler::quotation_state_tests" 19 "本卡（19 条：转单 + 状态机 + 审批闸 + 清场）"
run_group "business::erp::sales::quotation" 50 "报价单整组（M3-02 CRUD/状态机 + M3-03 转单 + M3-04 审批桥 + 本卡）"
run_group "business::erp::sales" 170 "销售域整组（订单 / 退货 / 目标 / 审批桥未被本卡打破）"

# ---------- 3) 夹具清场 ----------
echo "== 3) 夹具清场（不允许把 M305- 残留带出本组）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code = 'M305-CUST'")" "biz_customer 无 M305 夹具客户"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_quotation q JOIN biz_customer c ON c.id = q.customer_id WHERE c.code = 'M305-CUST'")" "biz_quotation 无 M305 夹具报价单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE flow_code = 'm305_fixture_flow'")" "无 M305 夹具审批实例"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_task t JOIN biz_flow_instance i ON i.id = t.instance_id WHERE i.flow_code = 'm305_fixture_flow'")" "无 M305 夹具审批待办"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_status_log WHERE entity_type = 'QUOTATION' AND entity_id NOT IN (SELECT id FROM biz_quotation)")" "无报价单孤儿状态日志（fixture_hygiene::SALES）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_status_log WHERE entity_type = 'SALES_ORDER' AND entity_id NOT IN (SELECT id FROM biz_sales_order)")" "无销售订单孤儿状态日志（fixture_hygiene::SALES）"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
