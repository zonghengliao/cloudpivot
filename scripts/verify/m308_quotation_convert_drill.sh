#!/usr/bin/env bash
# ============================================================================
# M3-08 报价单 → 销售订单「转单」单测 · 真库演练（含部分转单）
#
# 卡：t_8d301195（M3-08【M3·销售】报价单转订单：单测 / 含部分转单）
# 上游：t_6a61726c（M3-01 表）/ t_0750ad31（M3-02 CRUD+状态机）
#       t_de446ead（M3-03 转单主链）/ t_6bede75a（M3-04 审批闸）
#       t_a2ba25ed（M3-05 报价单单测）/ t_3ae6db9c（M3-06 前端）/ t_53f01475（M3-07 PDF）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 补模板落后的业务迁移 V0405…V0439（V0432 表 / V0433+V0434 权限码 / V0435 流程 / V0436 菜单）
#   3. 断言表与权限码、流程种子就位（迁移没跑成功时用例会以「表不存在」红，
#      先在这里一眼看出是环境问题不是代码问题）
#   4. 真库用例（--test-threads=1）：本卡 `quotation_convert_tests` 全绿 + **SKIP=0**
#      （真库用例被「无 ERP_DB_URL」打成 SKIP 就是假绿，必须断言）
#   5. 回归：报价单整组 + 销售域整组不许被本卡打破
#   6. 断言夹具清场：`M308-CUST` / 夹具账号 / 注入故障的触发器与函数 一律 0 残留，
#      销售链无孤儿状态日志
#   7. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m308_quotation_convert_drill.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m308_quotation_convert_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m308_quotation_convert_drill.sh          # 留库排查（打印库名）
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
DB="cp_m308_convert_drill_$$"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
LOG="$TMP/m308_drill_$$.log"
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
assert_le() { # 上限 实际 说明
    if [ "${2:-0}" -le "$1" ] 2>/dev/null; then ok "$3（=$2 ≤ 基线 $1）"; else bad "$3（期望 ≤ $1，实际 [$2]）"; fi
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

printf '== M3-08 报价单 → 销售订单「转单」单测 · 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 补齐迁移 V0405…V0439 ----------
echo "== 1) 补齐业务迁移 V0405…V0439（幂等，逐条 ON_ERROR_STOP）"
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
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_indexes WHERE tablename = 'biz_quotation' AND indexname = 'uk_biz_quotation_converted_so'")" "转单反向唯一索引在位（V0432）"
# 孤儿状态日志基线（模板库可能自带历史孤儿）—— 跑完只许「不多一条」
ORPHANS_BASELINE="$(psql_q "SELECT (SELECT count(*) FROM biz_status_log WHERE entity_type = 'QUOTATION' AND entity_id NOT IN (SELECT id FROM biz_quotation)) + (SELECT count(*) FROM biz_status_log WHERE entity_type = 'SALE_ORDER' AND entity_id NOT IN (SELECT id FROM biz_sales_order))")"
info "孤儿状态日志基线 = ${ORPHANS_BASELINE:-?}（QUOTATION + SALE_ORDER）"

# ---------- 2) 真库用例 ----------
# ⚠ ERP_DB_URL / SYS_DB_URL 必须先导出：写漏会打一排 SKIP 然后「0 失败」= 假绿，
# 所以下面同时断言 SKIP=0。
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
echo "== 2) 真库用例（--test-threads=1）"

run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m308_$$_${module//[:/]/_}.out"
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

run_group "business::erp::sales::quotation_handler::quotation_convert_tests" 21 "本卡（21 条：入参闸含部分转单 + 行级复制 + 原子性 + 幂等 + 清场）"
run_group "business::erp::sales::quotation" 70 "报价单整组（M3-02 CRUD/状态机 + M3-03 转单 + M3-04 审批桥 + M3-05 状态机 + 本卡）"
run_group "business::erp::sales" 190 "销售域整组（订单 / 退货 / 目标 / 审批桥未被本卡打破）"

# ---------- 3) 夹具清场 ----------
echo "== 3) 夹具清场（不允许把 M308- 残留 / 故障触发器带出本组）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code = 'M308-CUST'")" "biz_customer 无 M308 夹具客户"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_user WHERE username LIKE 'm308_sales%'")" "sys_user 无 M308 夹具账号"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_quotation q JOIN biz_customer c ON c.id = q.customer_id WHERE c.code = 'M308-CUST'")" "biz_quotation 无 M308 夹具报价单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order o JOIN biz_customer c ON c.id = o.customer_id WHERE c.code = 'M308-CUST'")" "biz_sales_order 无 M308 夹具订单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM pg_constraint WHERE conname = 'ck_m308_block_convert' AND conrelid = 'biz_status_log'::regclass")" "注入故障的 CHECK 约束已拆"
# 孤儿状态日志：与基线比对（跑测试不许**新增**孤儿；模板自带的历史孤儿不归本卡管）
ORPHANS_AFTER="$(psql_q "SELECT (SELECT count(*) FROM biz_status_log WHERE entity_type = 'QUOTATION' AND entity_id NOT IN (SELECT id FROM biz_quotation)) + (SELECT count(*) FROM biz_status_log WHERE entity_type = 'SALE_ORDER' AND entity_id NOT IN (SELECT id FROM biz_sales_order))")"
assert_le "${ORPHANS_BASELINE:-0}" "${ORPHANS_AFTER:-0}" "本卡跑完没有新增孤儿状态日志（QUOTATION + SALE_ORDER；本组 zz 收尾会顺手收干历史孤儿）"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
