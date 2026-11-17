#!/usr/bin/env bash
# ============================================================================
# M3-11 客户信用「超信用拦截（按配置策略执行）」· 真库演练
#
# 卡：t_8a08ce48（M3-11【M3·销售】客户信用：超信用拦截）
# 上游：t_464b7265（M3-09 策略三态 BLOCK/WARN/PASS + 提交守卫）、
#       t_8729b656（M3-10 实时快照 / 可用额度）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 补模板落后的业务迁移 V0405…V0437（本卡**不加迁移**：无 DDL）
#   3. 静态断言：审批闸接线（handler → doc_reader 注册表 → ERP 桥）、
#      `flow` 不直连 `erp`、拦截点只有两处（审批前闸 + 过审兜底）
#   4. 死权限码常驻闸 `perm_menu_gate.py`（本卡不造新码，必须仍为 0）
#   5. 真库用例（--test-threads=1），**SKIP=0** 逐组断言（真库用例被打成 SKIP 就是假绿）：
#        · credit 域（assess_order 取数口径 / 三态策略 / 不控额度 / 查不到）
#        · sales_approval_bridge 域（审批前闸 409·待办不动 / WARN 台账 / PASS 静默 /
#          过审兜底打回草稿·可重提 / 注册表 17 条非销售线放行）
#        · sales 域整组（订单提交守卫 + 报价单 / 退货 / 审批桥不被打破）
#        · flow 域整组（**含架构门禁**：flow→erp 直连引用必须仍为 0，
#          以及 `guard_approve_for_task` 扇出单测）
#   6. 断言夹具清场：CR38- / SO6HT-CUST / 信用台账 0 残留
#   7. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m311_credit_block_approval.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m311_credit_block_approval.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m311_credit_block_approval.sh          # 留库排查
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
DB="cp_m311_credit_block_$$"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m311_drill_$$.log"
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

printf '== M3-11 超信用拦截（按配置策略执行）· 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 补齐迁移 V0405…V0437（本卡无新迁移） ----------
echo "== 1) 补齐业务迁移 V0405…V0437（幂等，逐条 ON_ERROR_STOP；本卡**不加迁移**）"
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
assert_eq "2" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_customer' AND column_name IN ('credit_policy','credit_used')")" \
    "biz_customer 的 credit_policy / credit_used 都在（M3-09 + SO-38 的列）"
# 本卡不加迁移：下一个可用号仍是 V0438（不动 = 不与并行的卡抢号）
if ls "$MIG_DIR" | grep -qE '^V0438__'; then
    info "已存在 V0438（别的卡落的），本卡无新增迁移"
else
    ok "本卡未新增迁移（下一个可用号 V0438 未被本卡占用）"
fi

# ---------- 2) 静态断言：接线只有一条路 ----------
echo "== 2) 静态断言：审批闸接线（handler → doc_reader → ERP 桥）+ 不许直连 erp"
HANDLER="$ROOT/backend/app/src/business/flow/handler.rs"
DOC_READER="$ROOT/backend/app/src/business/flow/doc_reader.rs"
IMPL="$ROOT/backend/app/src/business/erp/doc_reader_impl.rs"
BRIDGE="$ROOT/backend/app/src/business/erp/sales/sales_approval_bridge.rs"

assert_eq "1" "$(grep -c 'doc_reader::guard_approve_for_task(task_id)' "$HANDLER")" \
    "approve_task 调注册表闸一次（重复调 = 同一张单判两遍）"
assert_eq "0" "$(grep -c 'erp::sales::sales_approval_bridge' "$HANDLER")" \
    "flow/handler 不直连 ERP 桥（架构门禁：flow→erp 直连基线 0）"
assert_eq "1" "$(grep -c 'pub async fn guard_approve_for_task' "$DOC_READER")" \
    "doc_reader 提供 guard_approve_for_task 扇出"
assert_eq "1" "$(grep -c 'async fn guard_approve(&self, _task_id: Uuid)' "$DOC_READER")" \
    "trait 的 guard_approve 有默认放行实现（其余 17 条审批线不写噪音）"
assert_eq "1" "$(grep -c 'ErpDoc::SoApprove => sales_approval_bridge::guard_credit_on_approve' "$IMPL")" \
    "18 条审批线里只有销售订单接额度闸（多接一条 = 改变生产行为）"
assert_eq "1" "$(grep -c 'pub async fn guard_credit_on_approve' "$BRIDGE")" \
    "ERP 桥提供审批点同意前闸"
assert_eq "1" "$(grep -c 'pub async fn assess_order' "$ROOT/backend/app/src/business/erp/credit.rs")" \
    "credit::assess_order 是审批闸与过审兜底的唯一取数口"
assert_eq "2" "$(grep -c 'credit::assess_order(db(), order_id)' "$BRIDGE")" \
    "审批前闸 + 过审兜底各判一次（判定同一份 decide，取数同一口）"
assert_eq "1" "$(grep -c 'assessed.compensation_reason()' "$BRIDGE")" \
    "BLOCK 时的补偿路径只走 compensation_reason（打回草稿，不留死单）"

# ---------- 3) 死权限码常驻闸（本卡不造新码） ----------
echo "== 3) perm_menu_gate.py（注解码 − 菜单码 必须为 0）"
if NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT="$PG_PORT" NIK_DATABASE_USER="$PG_USER" \
   NIK_DATABASE_NAME="$DB" NIK_DATABASE_PASSWORD="$PGPASSWORD" \
   python3 "$ROOT/scripts/verify/perm_menu_gate.py" >"$TMP/m311_perm_gate_$$.log" 2>&1; then
    ok "死权限码闸通过（$(grep -c . "$TMP/m311_perm_gate_$$.log") 行输出，见日志）"
else
    bad "死权限码闸红：$(tail -5 "$TMP/m311_perm_gate_$$.log" | tr '\n' ' ')"
fi
rm -f "$TMP/m311_perm_gate_$$.log"

# ---------- 4) 真库用例 ----------
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
echo "== 4) 真库用例（--test-threads=1）"

run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m311_$$_${module//[:/]/_}.out"
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
        ok "$label：本组 SKIP=0（真库用例没被打成假绿）"
    else
        bad "$label：本组出现 $skipped 行 SKIP（真库用例被跳过 = 假绿，见 $out）"
    fi
}

run_group "business::erp::credit" 30 "信用域（assess_order 取数口径 + 三态策略 + 不控额度 + 查不到）"
run_group "business::erp::sales::sales_approval_bridge" 14 "审批桥（前闸 409·待办不动 / WARN 台账 / PASS 静默 / 过审兜底打回草稿·可重提）"
run_group "business::erp::sales" 211 "销售域整组（订单提交守卫 + 报价单 / 退货 / 审批桥不被打破）"
run_group "business::flow" 433 "流程域整组（含架构门禁 flow→erp=0 + guard_approve_for_task 扇出）"

# ---------- 5) 夹具清场 ----------
echo "== 5) 夹具清场（不允许把本卡夹具带出演练库）"
psql_exec() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -c "$1" >/dev/null 2>&1; }
# 审批桥 / 信用域的用例约定是「setup 先清、跑完不清」（cleanup-at-start，与全仓一致），
# 所以**最后一个用例**的夹具会留在库里。演练脚本按同一份口径清尾，再断言库里干净 ——
# 断言的对象是「清尾 SQL 有效 + 本卡没漏新的表」，不是「用例自己会清」。
info "清尾前残留：SO6HT-CUST=$(psql_q "SELECT count(*) FROM biz_customer WHERE code='SO6HT-CUST'") · CR38-=$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE 'CR38-%'")（用例 cleanup-at-start 的已知形态）"
psql_exec "DELETE FROM biz_flow_task WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE')"
psql_exec "DELETE FROM biz_flow_instance_node WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE')"
psql_exec "DELETE FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE'"
psql_exec "DELETE FROM biz_credit_log WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'CR38-%' OR code = 'SO6HT-CUST')"
psql_exec "DELETE FROM biz_status_log WHERE entity_type = 'SALE_ORDER' AND entity_id IN (SELECT o.id FROM biz_sales_order o JOIN biz_customer c ON c.id = o.customer_id WHERE c.code LIKE 'CR38-%' OR c.code = 'SO6HT-CUST')"
psql_exec "DELETE FROM biz_sales_order WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'CR38-%' OR code = 'SO6HT-CUST')"
psql_exec "DELETE FROM biz_receivable WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'CR38-%' OR code = 'SO6HT-CUST')"
psql_exec "DELETE FROM biz_sales_return WHERE customer_id IN (SELECT id FROM biz_customer WHERE code LIKE 'CR38-%' OR code = 'SO6HT-CUST')"
psql_exec "DELETE FROM sys_user_role WHERE user_id = 'd6281000-0000-4000-8000-000000000001'::uuid"
psql_exec "DELETE FROM sys_user WHERE id = 'd6281000-0000-4000-8000-000000000001'::uuid"
psql_exec "DELETE FROM biz_customer WHERE code LIKE 'CR38-%' OR code = 'SO6HT-CUST'"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE 'CR38-%'")" "biz_customer 无 CR38- 夹具客户（credit 域）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code = 'SO6HT-CUST'")" "biz_customer 无 SO6HT-CUST 审批夹具客户"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE so_no LIKE 'SO38-%' OR so_no LIKE 'SO6HT-%'")" "biz_sales_order 无夹具订单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE'")" "biz_flow_instance 无 SO_APPROVE 夹具实例"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_credit_log l LEFT JOIN biz_customer c ON c.id = l.customer_id WHERE c.code LIKE 'CR38-%' OR c.code = 'SO6HT-CUST'")" "信用台账无夹具客户残留行"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE status = 'SUBMITTED'")" "演练库不留「审批中」死单"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
