#!/usr/bin/env bash
# ============================================================================
# M4-20 采购缺口 E2E（计划 → 申请 → 订单 → 入库）· 真库演练
#
# 卡：t_6a4a60ba（M4-20【M4·采购】采购缺口 E2E：计划 → 申请 → 订单 → 入库）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 e2e，脚本自己 DROP；
#      给了 `M420_DB` 则只跑不清 —— 用 OWN_DB 标记位区分「自己建的库」）
#   2. 补模板落后的业务迁移 V0405…最新（模板库只停在 V0404；本链要的
#      `biz_purchase_plan`(V0446) / `pp_approve` 流程(V0447) / 计划执行率视图(V0449)
#      全在这个区间里）
#   3. 前置断言：表 / 流程种子 / 角色 / 科目 / 状态机边 —— 环境缺件时先在这里
#      一眼看出，不让它伪装成「用例红」
#   4. 演练库清污（**只针对模板库自带的一条历史残留**，见 §3 注释）
#   5. 真库用例（--test-threads=1）：本卡 `purchase_chain_e2e_tests` 全绿 + **SKIP=0**
#      （真库用例被「无 ERP_DB_URL」打成 SKIP 就是假绿，必须断言）
#   6. 回归：采购域 + 收货域整组不许被本卡打破
#   7. 夹具清场断言：`M420-` 计划 / 申请 / 订单 / 收货单 / 审批实例 / 凭证 / 应付 /
#      三本账 一律 0 残留
#   8. 打印 PASS/FAIL 汇总；任一不符退出码 1（环境问题 2）
#
# 用法：
#   bash scripts/verify/m420_purchase_chain_e2e_drill.sh                    # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m420_purchase_chain_e2e_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m420_purchase_chain_e2e_drill.sh          # 留库排查（打印库名）
#   TEETH=1 bash scripts/verify/m420_purchase_chain_e2e_drill.sh            # 有牙自检（坏连接串必须红）
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
DB="${M420_DB:-cp_m420_purchase_chain_e2e_$$}"
KEEP_DB="${KEEP_DB:-0}"
TEETH="${TEETH:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m420_drill_$$.log"
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

# 自己建的库才由自己删；`M420_DB` 是调用方给的库 → 只跑不清
OWN_DB=1
if [ -n "${M420_DB:-}" ]; then OWN_DB=0; fi

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

printf '== M4-20 采购缺口 E2E（计划 → 申请 → 订单 → 入库）演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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
echo "== 1) 补齐业务迁移 V0405…最新（幂等，逐条 ON_ERROR_STOP）"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    ver="${f:1:4}"
    # 模板库停在 V0404：只补它之后的（V0405 起）
    if [ "$((10#$ver))" -le 404 ]; then continue; fi
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
echo "== 2) 前置：本链依赖的表 / 流程 / 角色 / 科目 / 状态机边"
assert_eq "biz_purchase_plan" "$(psql_q "SELECT to_regclass('public.biz_purchase_plan')")" "采购计划头表存在（V0446）"
assert_eq "biz_purchase_plan_item" "$(psql_q "SELECT to_regclass('public.biz_purchase_plan_item')")" "采购计划明细表存在（V0446）"
assert_eq "biz_purchase_requisition" "$(psql_q "SELECT to_regclass('public.biz_purchase_requisition')")" "采购申请表存在（V0331）"
assert_eq "biz_purchase_order" "$(psql_q "SELECT to_regclass('public.biz_purchase_order')")" "采购订单表存在（V0137/V0145）"
assert_eq "biz_stock_in" "$(psql_q "SELECT to_regclass('public.biz_stock_in')")" "收货入库表存在（V0145）"
assert_eq "biz_payable" "$(psql_q "SELECT to_regclass('public.biz_payable')")" "应付表存在（V0161）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'pp_approve' AND status = 'PUBLISHED' AND deleted_at IS NULL")" "pp_approve 已发布（V0447）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'pr_approve' AND status = 'PUBLISHED' AND deleted_at IS NULL")" "pr_approve 已发布（V0331）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_flow_def WHERE flow_code = 'po_approve' AND status = 'PUBLISHED' AND deleted_at IS NULL")" "po_approve 已发布（V0182）"
assert_eq "5" "$(psql_q "SELECT count(*) FROM sys_role WHERE role_code IN ('PURCHASE_MANAGER','FINANCE_MANAGER','DEPT_LEADER','GENERAL_MANAGER','WAREHOUSE_MANAGER') AND deleted_at IS NULL")" "三条流程用到的五个岗位都在位"
assert_eq "3" "$(psql_q "SELECT count(*) FROM biz_account WHERE account_code IN ('1405','2202','2221.01') AND status = 1")" "进项凭证三个默认科目齐全（1405 / 2202 / 2221.01）"
# 状态机边：`biz_flow_state_edge` **只存「被人为改过」的边**，没有行 = 用代码里的默认边表。
# 所以这里只能断言「没有被关掉的边」，不能断言「每条边都有一行」—— 那会把默认态误判成缺件。
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_state_edge WHERE entity IN ('PURCHASE_ORDER','STOCK_IN','PURCHASE_RETURN','STOCK_OUT') AND enabled = false")" "采购链四张单据没有被关掉的状态机边"

# ---------- 3) 演练库清污（只清模板库自带的一条历史残留） ----------
#
# 为什么要这一步：模板库 `cloudpivot_test` 自带的演示数据里有**一条**2026-11-10 的
# 历史 `PO_APPROVE | PENDING` 实例（订单号 `W1B-PO-0001`），而采购域收尾用例
# `purchase_handler::tests::zz_teardown_purges_po_approval_residue` 的断言口径是
# 「**全库**不剩在跑的采购审批实例」—— 它会红在这条与本卡毫无关系的历史残留上
# （实测：不清理时 453 passed / 1 failed，清理后 454 passed / 0 failed）。
# 所以只按**单号白名单**删这条残留，并把它打印出来；任何别的失败仍然会让演练红。
echo "== 3) 演练库清污：模板自带的 W1B-PO-0001 历史 PENDING 审批实例"
STALE_TASKS="$(psql_q "WITH stale AS (SELECT i.id FROM biz_flow_instance i JOIN biz_purchase_order po ON po.id = i.biz_id WHERE i.biz_type = 'PO_APPROVE' AND i.status = 'PENDING' AND i.deleted_at IS NULL AND po.order_no = 'W1B-PO-0001') DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM stale)")"
STALE_NODES="$(psql_q "WITH stale AS (SELECT i.id FROM biz_flow_instance i JOIN biz_purchase_order po ON po.id = i.biz_id WHERE i.biz_type = 'PO_APPROVE' AND i.status = 'PENDING' AND i.deleted_at IS NULL AND po.order_no = 'W1B-PO-0001') DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM stale)")"
STALE_INST="$(psql_q "DELETE FROM biz_flow_instance i USING biz_purchase_order po WHERE po.id = i.biz_id AND i.biz_type = 'PO_APPROVE' AND i.status = 'PENDING' AND i.deleted_at IS NULL AND po.order_no = 'W1B-PO-0001'")"
info "清污影响行数：待办 [$STALE_TASKS] / 节点 [$STALE_NODES] / 实例 [$STALE_INST]"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'PO_APPROVE' AND status = 'PENDING' AND deleted_at IS NULL AND biz_id IN (SELECT id FROM biz_purchase_order WHERE order_no = 'W1B-PO-0001')")" "W1B 历史残留已清（别的单号一行不碰）"

# ---------- 4) 有牙自检（可选）：坏连接串必须红 ----------
if [ "$TEETH" = "1" ]; then
    echo "== 4) 有牙自检：坏连接串必须让本卡用例红（否则「全绿」是假绿）"
    teeth_out="$TMP/m420_teeth_$$.out"
    ( cd "$BACKEND" && ERP_DB_URL="postgres://$PG_USER:wrongpass@$PG_HOST:$PG_PORT/cp_m420_no_such_db" \
        cargo test -p cloudpivot --lib business::erp::purchase::purchase_chain_e2e_tests -- --test-threads=1 ) >"$teeth_out" 2>&1
    teeth_rc=$?
    teeth_line="$(grep -E '^test result: ' "$teeth_out" | tail -1)"
    info "坏连接串跑本卡用例：rc=$teeth_rc / $teeth_line"
    if [ "$teeth_rc" -ne 0 ] && printf '%s' "$teeth_line" | grep -q '3 failed'; then
        ok "有牙：坏连接串下 3 条用例全部失败（拒绝静默 SKIP 假绿）"
    else
        bad "失牙：坏连接串下竟然没有 3 条失败（假绿风险）"
    fi
    printf '  ---- 原始输出：%s\n' "$teeth_out"
    printf '\n== 汇总（有牙模式）：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
    if [ "$FAIL" -eq 0 ]; then echo "ALL PASS"; exit 0; fi
    exit 1
fi

# ---------- 5) 真库用例 ----------
echo "== 5) 真库用例（--test-threads=1）"
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"

run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m420_$$_${module//[:/]/_}.out"
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
        # 失败定位三件套：断言原文 + 左/右值 + 步骤轨迹尾 10 行
        grep -E 'panicked at|assertion|left:|right:' -A 2 "$out" | head -40
        printf '  ---- 步骤轨迹（尾 10 行）：\n'
        grep -E '^\[M4-20\]' "$out" | tail -10 | sed 's/^/        /'
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

run_group "business::erp::purchase::purchase_chain_e2e_tests" 3 "本卡（全链 / 分批 / 清场 3 条）"
run_group "business::erp::purchase" 450 "采购域整组（计划 / 申请 / 订单 / 框架协议 / 合同未被本卡打破）"
run_group "business::erp::stockin" 160 "收货域整组（建单 / 过账 / 三本账 / 作废未被本卡打破）"

# ---------- 6) 夹具清场 ----------
echo "== 6) 夹具清场（M420- 一律 0 残留）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_plan WHERE plan_name LIKE 'M420-%'")" "biz_purchase_plan 无 M420 夹具计划"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_plan_item WHERE plan_id IN (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M420-%')")" "biz_purchase_plan_item 无 M420 明细"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_requisition WHERE source_plan_id IN (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M420-%')")" "biz_purchase_requisition 无 M420 申请单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE id IN (SELECT po_id FROM biz_purchase_requisition WHERE po_id IS NOT NULL AND source_plan_id IN (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M420-%'))")" "biz_purchase_order 无 M420 订单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_in WHERE supplier_id IN (SELECT id FROM biz_supplier WHERE supplier_code = 'M420-SUP')")" "biz_stock_in 无 M420 收货单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_payable WHERE supplier_id IN (SELECT id FROM biz_supplier WHERE supplier_code = 'M420-SUP')")" "biz_payable 无 M420 应付"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE biz_type IN ('PP_APPROVE','PR_APPROVE','PO_APPROVE') AND biz_id IN (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M420-%')")" "biz_flow_instance 无 M420 审批实例"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M420-M1','M420-M2'))")" "biz_inventory 无 M420 库存"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_location WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M420-M1','M420-M2'))")" "biz_inventory_location 无 M420 库位账"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M420-M1','M420-M2'))")" "biz_inventory_txn 无 M420 流水"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_user WHERE username LIKE 'm420\\_%'")" "sys_user 无 M420 夹具账号"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
