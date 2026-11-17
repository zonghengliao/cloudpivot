#!/usr/bin/env bash
# ============================================================================
# M3-25 销售缺口 E2E（信用控制 · 超限场景）· 真库演练
#
# 卡：t_76dcfc44（M3-25【M3·销售】销售缺口 E2E：信用控制 / 超限场景）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 e2e，脚本自己 DROP；
#      给了 `M325_DB_URL` 则只跑不清 —— 用 OWN_DB 标记位区分「自己建的库」）
#   2. 补模板落后的业务迁移 V0405…V0445（模板库只到 V0404；本链要的
#      `credit_policy`(V0437) / 在途账(V0420) / 报价单(V0432) 全在这个区间里）
#   3. 前置断言：表 / 额度列与 CHECK / 流程种子 / 角色 / 状态机边 / 号段 ——
#      环境缺件时先在这里一眼看出，不让它伪装成「用例红」
#   4. 真库用例（--test-threads=1 **且 --nocapture**）：本卡
#      `credit_control_e2e_tests` 全绿 + **SKIP=0**
#      （--nocapture 是必须的：Rust test harness 默认捕获通过用例的 stdout，
#       不带它时「SKIP: ERP_DB_URL 连接失败」会被吞掉，grep '^SKIP' 永远是 0 = 假绿）
#   5. 回归：信用域 / 销售域 / 发货域整组不许被本卡打破
#   6. 夹具清场断言：`M325-` 客户 / 账号 / 订单 / 发货 / 出库 / 应收 / 收款 /
#      审批实例 / 凭证 / 信用台账 / 三本账 / 在途账 一律 0 残留
#   7. 打印 PASS/FAIL 汇总；任一不符退出码 1（环境问题 2）
#
# 用法：
#   bash scripts/verify/m325_credit_control_e2e_drill.sh                    # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m325_credit_control_e2e_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m325_credit_control_e2e_drill.sh          # 留库排查（打印库名）
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
DB="${M325_DB:-cp_m325_credit_e2e_$$}"
KEEP_DB="${KEEP_DB:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m325_drill_$$.log"
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

# 自己建的库才由自己删；`M325_DB` 是调用方给的库 → 只跑不清
OWN_DB=1
if [ -n "${M325_DB:-}" ]; then OWN_DB=0; fi

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

printf '== M3-25 销售缺口 E2E（信用控制 · 超限场景）演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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
echo "== 2) 前置：本链依赖的表 / 额度列 / 流程 / 角色 / 状态机边"
assert_eq "biz_credit_log" "$(psql_q "SELECT to_regclass('public.biz_credit_log')")" "信用台账表存在（V0313）"
assert_eq "biz_inventory_in_transit" "$(psql_q "SELECT to_regclass('public.biz_inventory_in_transit')")" "在途账表存在（V0420）"
assert_eq "biz_receipt_allocation" "$(psql_q "SELECT to_regclass('public.biz_receipt_allocation')")" "核销明细表存在（M3-12 依赖）"
assert_eq "credit_policy" "$(psql_q "SELECT COALESCE((SELECT column_name FROM information_schema.columns WHERE table_name='biz_customer' AND column_name='credit_policy'),'')")" "客户表有 credit_policy 列（V0437）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM information_schema.constraint_column_usage WHERE table_name='biz_customer' AND constraint_name='ck_biz_customer_credit_policy'")" "credit_policy 的白名单 CHECK 在位（BLOCK/WARN/PASS）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'so_approve' AND status = 'PUBLISHED' AND deleted_at IS NULL")" "so_approve 已发布（V0281）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM sys_role WHERE role_code = 'SALES_SUPERVISOR' AND deleted_at IS NULL")" "SALES_SUPERVISOR 角色在位（审批第一跳）"
# 状态机边：`biz_flow_state_edge` **只存「被人为改过」的边**，没有行 = 用代码里的默认边表
# （全允许）。所以只断言「没有被关掉的边」，不能断言「这条边有一行」。
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_state_edge WHERE entity IN ('STOCK_OUT','SHIPMENT','SALE_ORDER','RECEIVABLE','RECEIPT') AND enabled = false")" "信用链五张单据没有被关掉的状态机边"

# ---------- 3) 真库用例 ----------
echo "== 3) 真库用例（--test-threads=1 --nocapture）"
export ERP_DB_URL="postgres://$PG_USER:${PGPASSWORD}@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"

# SKIP 自检（有牙证明）：故意把连接串换成错的，期望 run_group 立刻报「SKIP=1 + 缺正向标记」。
# 用法：M325_BAD_URL=1 bash scripts/verify/m325_credit_control_e2e_drill.sh
# 存在的理由：真库用例「连不上 → 打印 SKIP → 早退」在 Rust test harness 里仍算 `ok`，
# 只数 passed 会让假绿看起来全绿；这一档把「守卫到底咬不咬」变成可复现的一次运行。
if [ "${M325_BAD_URL:-}" = "1" ]; then
    export ERP_DB_URL="postgres://$PG_USER:definitely-wrong-password@$PG_HOST:$PG_PORT/no_such_db_skip_guard"
    export SYS_DB_URL="$ERP_DB_URL"
    info "SKIP 自检模式（M325_BAD_URL=1）：连接串故意写错，期望下面出现 SKIP>0 与「缺正向标记」的 FAIL"
fi

run_group() { # 模块路径 最少用例数 说明 [正向标记]
    local module="$1" min="$2" label="$3" marker="${4:-}" out line passed failed skipped
    out="$TMP/m325_$$_${module//[:/]/_}.out"
    ( cd "$BACKEND" && cargo test -p cloudpivot --lib "$module" -- --test-threads=1 --nocapture ) >"$out" 2>&1
    line="$(grep -E '^test result: ' "$out" | tail -1)"
    passed="$(printf '%s' "$line" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
    # SKIP 判定用 `SKIP:` **不带行首锚点**：`--nocapture` 下测试名与 SKIP 打在同一行
    # （`test xxx ... SKIP: ERP_DB_URL 连接失败`），`grep '^SKIP'` 会数成 0 = 假绿。
    skipped="$(grep -c 'SKIP:' "$out" || true)"
    [ -z "${passed:-}" ] && passed=0
    [ -z "${failed:-}" ] && failed=0
    [ -z "${skipped:-}" ] && skipped=0
    if [ "${failed:-0}" -eq 0 ] && [ "${passed:-0}" -ge "$min" ]; then
        ok "$label：$module 全绿（$passed 条）"
    elif [ "${failed:-0}" -ne 0 ]; then
        bad "$label：$module 有 $failed 条失败（见 $out）"
        # 失败定位三件套：断言原文 + 左/右值 + 步骤轨迹尾 10 行
        grep -E 'panicked at|assertion|left:|right:' -A 2 "$out" | head -40
        printf '  ---- 步骤轨迹（尾 10 行）：\n'
        grep -E '^\[M3-25\]' "$out" | tail -10 | sed 's/^/        /'
    else
        bad "$label：$module 只跑了 $passed 条（期望 ≥ $min，见 $out）"
    fi
    if [ "$skipped" -eq 0 ]; then
        ok "$label：本组 SKIP=0（真库用例没被「无 ERP_DB_URL」打成假绿）"
    else
        bad "$label：本组出现 $skipped 行 SKIP（真库用例被跳过 = 假绿）"
        grep 'SKIP:' "$out" | head -5 | sed 's/^/        /'
    fi
    # 正向控制：真跑过的用例一定有步骤轨迹；连轨迹都没有说明整组被跳过（哪怕 SKIP 文案被吞）
    if [ -n "$marker" ]; then
        if grep -qF "$marker" "$out"; then
            ok "$label：正向标记在位（$marker）"
        else
            bad "$label：缺正向标记「$marker」—— 说明用例没真跑（被跳过 / 被过滤）"
        fi
    fi
    printf '  ---- 原始输出：%s\n' "$out"
}

run_group "business::erp::sales::credit_control_e2e_tests" 2 "本卡（超限全链 1 条 + 收尾 1 条）" "[M3-25] 全链通过"
run_group "business::erp::credit" 40 "信用域整组（占用 / 释放 / 策略 / 快照 / 并发未被本卡打破）"
run_group "business::erp::sales" 300 "销售域整组（订单 / 报价 / 退货 / 目标 / 预测未被本卡打破）"
run_group "business::erp::shipment" 40 "发货域整组（过账 / 签收 / 在途未被本卡打破）"

# ---------- 4) 夹具清场 ----------
echo "== 4) 夹具清场（M325- 一律 0 残留）"
M325_CUST="(SELECT id FROM biz_customer WHERE code = 'M325-CUST')"
M325_ORDERS="(SELECT id FROM biz_sales_order WHERE customer_id IN $M325_CUST)"
M325_SHIPS="(SELECT id FROM biz_shipment WHERE so_id IN $M325_ORDERS)"
M325_OUTS="(SELECT id FROM biz_stock_out WHERE source_id IN $M325_SHIPS)"
M325_RECEIPTS="(SELECT id FROM biz_receipt WHERE customer_id IN $M325_CUST)"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code = 'M325-CUST'")" "biz_customer 无 M325 夹具客户"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_user WHERE username LIKE 'm325\\_%'")" "sys_user 无 M325 夹具账号"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE customer_id IN $M325_CUST")" "biz_sales_order 无 M325 夹具订单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_shipment WHERE so_id IN $M325_ORDERS")" "biz_shipment 无 M325 夹具发货单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_out WHERE source_id IN $M325_SHIPS")" "biz_stock_out 无 M325 夹具出库单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE' AND biz_id IN $M325_ORDERS")" "biz_flow_instance 无 M325 夹具审批实例"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN $M325_OUTS")" "biz_voucher 无 M325 夹具凭证"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_receivable WHERE customer_id IN $M325_CUST")" "biz_receivable 无 M325 夹具应收"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_receipt WHERE customer_id IN $M325_CUST")" "biz_receipt 无 M325 夹具收款"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_receipt_allocation WHERE receipt_id IN $M325_RECEIPTS")" "biz_receipt_allocation 无 M325 夹具核销明细"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_credit_log WHERE customer_id IN $M325_CUST")" "biz_credit_log 无 M325 夹具台账"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M325-M1','M325-M2'))")" "biz_inventory 无 M325 夹具库存"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_location WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M325-M1','M325-M2'))")" "biz_inventory_location 无 M325 夹具库位账"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M325-M1','M325-M2'))")" "biz_inventory_txn 无 M325 夹具流水"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M325-M1','M325-M2'))")" "biz_inventory_in_transit 无 M325 夹具在途行"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
