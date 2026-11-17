#!/usr/bin/env bash
# ============================================================================
# M3-24 销售缺口 E2E（报价单 → 订单 → 出库）· 真库演练
#
# 卡：t_367777a2（M3-24【M3·销售】销售缺口 E2E：报价单 → 订单 → 出库）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 e2e，脚本自己 DROP；
#      给了 `M324_DB_URL` 则只跑不清 —— 用 OWN_DB 标记位区分「自己建的库」）
#   2. 补模板落后的业务迁移 V0405…V0445（模板库只到 V0404；本链要的
#      `biz_quotation`(V0432) / `quotation_approve`(V0435) / `biz_inventory_in_transit`(V0420)
#      全在这个区间里）
#   3. 前置断言：表 / 流程种子 / 角色 / 状态机边 —— 环境缺件时先在这里一眼看出，
#      不让它伪装成「用例红」
#   4. 真库用例（--test-threads=1）：本卡 `sales_chain_e2e_tests` 全绿 + **SKIP=0**
#      （真库用例被「无 ERP_DB_URL」打成 SKIP 就是假绿，必须断言）
#   5. 回归：销售域整组不许被本卡打破
#   6. 夹具清场断言：`M324-` 客户 / 账号 / 报价 / 订单 / 发货 / 出库 / 审批实例 /
#      凭证 / 应收 / 三本账 / 在途账 一律 0 残留
#   7. 打印 PASS/FAIL 汇总；任一不符退出码 1（环境问题 2）
#
# 用法：
#   bash scripts/verify/m324_sales_chain_e2e_drill.sh                    # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m324_sales_chain_e2e_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m324_sales_chain_e2e_drill.sh          # 留库排查（打印库名）
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 e2e；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M324_DB:-cp_m324_sales_chain_e2e_$$}"
KEEP_DB="${KEEP_DB:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m324_drill_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

# 库名护栏（任何写操作之前）：只许碰「演练库」形态的名字
case "$DB" in
    *drill*|*e2e*|*test*) : ;;
    *) printf '库名 %s 不含 drill/e2e/test，拒绝在它上面跑（rc=2）\n' "$DB"; exit 2 ;;
esac

# 自己建的库才由自己删；`M324_DB` 是调用方给的库 → 只跑不清
OWN_DB=1
if [ -n "${M324_DB:-}" ]; then OWN_DB=0; fi

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

printf '== M3-24 销售缺口 E2E（报价单 → 订单 → 出库）演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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
echo "== 1) 补齐业务迁移 V0405…V0445（幂等，逐条 ON_ERROR_STOP）"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V04(0[5-9]|[1-4][0-9])__.*\.up\.sql$' | sort); do
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条业务迁移全部应用成功"
fi

# ---------- 2) 前置断言（环境缺件 ≠ 用例红） ----------
echo "== 2) 前置：本链依赖的表 / 流程 / 角色 / 状态机边"
assert_eq "biz_quotation" "$(psql_q "SELECT to_regclass('public.biz_quotation')")" "报价单头表存在（V0432）"
assert_eq "biz_quotation_line" "$(psql_q "SELECT to_regclass('public.biz_quotation_line')")" "报价单明细表存在（V0432）"
assert_eq "biz_inventory_in_transit" "$(psql_q "SELECT to_regclass('public.biz_inventory_in_transit')")" "在途账表存在（V0420）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'quotation_approve' AND status = 'PUBLISHED' AND deleted_at IS NULL")" "quotation_approve 已发布（V0435）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'so_approve' AND status = 'PUBLISHED' AND deleted_at IS NULL")" "so_approve 已发布（V0281）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_role WHERE role_code = 'SALES_SUPERVISOR' AND deleted_at IS NULL")" "SALES_SUPERVISOR 角色在位（两条流程第一跳）"
# 状态机边：`biz_flow_state_edge` **只存「被人为改过」的边**，没有行 = 用代码里的
# `state_machine::*_EDGES` 默认边表（全允许）。所以这里只断言「没有被关掉的边」，
# 不能断言「这条边有一行」—— 那会把默认态误判成缺件。
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_state_edge WHERE entity IN ('STOCK_OUT','SHIPMENT','QUOTATION','SALE_ORDER') AND enabled = false")" "销售链四张单据没有被关掉的状态机边"

# ---------- 3) 真库用例 ----------
echo "== 3) 真库用例（--test-threads=1）"
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"

run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m324_$$_${module//[:/]/_}.out"
    ( cd "$BACKEND" && cargo test -p cloudpivot --lib "$module" -- --test-threads=1 ) >"$out" 2>&1
    line="$(grep -E '^test result: ' "$out" | tail -1)"
    passed="$(printf '%s' "$line" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
    skipped="$(grep -c '^SKIP' "$out" || true)"
    [ -z "${passed:-}" ] && passed=0
    [ -z "${failed:-}" ] && failed=0
    [ -z "${skipped:-}" ] && skipped=0
    if [ "${failed:-0}" -eq 0 ] && [ "${passed:-0}" -ge "$min" ]; then
        ok "$label：$module 全绿（$passed 条）"
    elif [ "${failed:-0}" -ne 0 ]; then
        bad "$label：$module 有 $failed 条失败（见 $out）"
        # 失败定位三件套：断言原文 + 左/右值 + 步骤轨迹尾 10 行（卡面「失败能定位」的落点）
        grep -E 'panicked at|assertion|left:|right:' -A 2 "$out" | head -40
        printf '  ---- 步骤轨迹（尾 10 行）：\n'
        grep -E '^\[M3-24\]' "$out" | tail -10 | sed 's/^/        /'
    else
        bad "$label：$module 只跑了 $passed 条（期望 ≥ $min，见 $out）"
    fi
    if [ "$skipped" -eq 0 ]; then
        ok "$label：本组 SKIP=0（真库用例没被「无 ERP_DB_URL」打成假绿）"
    else
        bad "$label：本组出现 $skipped 行 SKIP（真库用例被跳过 = 假绿）"
    fi
    printf '  ---- 原始输出：%s\n' "$out"
}

run_group "business::erp::sales::sales_chain_e2e_tests" 2 "本卡（全链 1 条 + 收尾 1 条）"
run_group "business::erp::sales" 300 "销售域整组（订单 / 报价 / 退货 / 目标 / 预测未被本卡打破）"
run_group "business::erp::shipment" 40 "发货域整组（过账 / 签收 / 在途未被本卡打破）"

# ---------- 4) 夹具清场 ----------
echo "== 4) 夹具清场（M324- 一律 0 残留）"
M324_ORDERS="(SELECT id FROM biz_sales_order WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = 'M324-CUST'))"
M324_SHIPS="(SELECT id FROM biz_shipment WHERE so_id IN $M324_ORDERS)"
M324_OUTS="(SELECT id FROM biz_stock_out WHERE source_id IN $M324_SHIPS)"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code = 'M324-CUST'")" "biz_customer 无 M324 夹具客户"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_user WHERE username LIKE 'm324\_%'")" "sys_user 无 M324 夹具账号"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = 'M324-CUST')")" "biz_sales_order 无 M324 夹具订单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_shipment WHERE so_id IN $M324_ORDERS")" "biz_shipment 无 M324 夹具发货单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_out WHERE source_id IN $M324_SHIPS")" "biz_stock_out 无 M324 夹具出库单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE biz_type IN ('QUOTATION_APPROVE','SO_APPROVE') AND biz_id IN $M324_ORDERS")" "biz_flow_instance 无 M324 夹具审批实例"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN $M324_OUTS")" "biz_voucher 无 M324 夹具凭证"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id IN $M324_OUTS")" "biz_receivable 无 M324 夹具应收"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M324-M1','M324-M2'))")" "biz_inventory 无 M324 夹具库存"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_location WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M324-M1','M324-M2'))")" "biz_inventory_location 无 M324 夹具库位账"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M324-M1','M324-M2'))")" "biz_inventory_txn 无 M324 夹具流水"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M324-M1','M324-M2'))")" "biz_inventory_in_transit 无 M324 夹具在途行"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
