#!/usr/bin/env bash
# ============================================================================
# M2-03 签收确认 → 在途转已出库 · 真库演练
#
# 卡：t_5f324a4c（M2-03【M2·仓库】在途库存：签收确认 → 在途转已出库）
# 上游：t_6c090f97（M2-02 发货过账 → 在途 + 出库待确认）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 补齐模板落后的业务迁移（V0405…V0420，全部幂等；V0420 建的正是在途账）
#      —— 不补就缺 `biz_voucher.source_doc_no`（V0419），任何出库过账都会 500
#   3. 跑真库用例（串行，共享夹具必须 --test-threads=1）：
#        business::erp::shipment::shipment_receipt_tests  ← 本卡主证据（7 条真库 + 3 条纯逻辑）
#        business::erp::shipment::shipment_posting        ← 上游口径未被破坏（在途 + 出库待确认）
#        business::erp::shipment::shipment_transit        ← 在途账写入口
#        business::erp::stockout                          ← 出库过账内核（⑤c 收口所在）
#        business::erp::receivable::receivable_bridge     ← 下游：签收确认才挂应收
#   4. 用例跑完后在库里复核「本卡口径」的落库形态（不依赖 Rust 断言）：
#        · 在途恒等式 / 状态×数量咬合 / RECEIVED 必有 received_at
#        · **不许**「出库单已过账但该发货单还有未签收在途」（扣了库存却挂账在途）
#        · **不许**「出库单待确认但该发货单在途已清零」（清了在途没扣库存）
#        · **不许**「出库单已作废（SALES）但该发货单还有未签收在途」（作废不释放在途，
#          可用量被永久占住 —— M2-FIX t_f123b57f 的缺口；取样前单独再跑一遍作废用例）
#        · 已签收的发货单：三本账确实扣了、成本结转凭证与应收各 1 条
#   5. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m203_shipment_receipt_drill.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m203_shipment_receipt_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m203_shipment_receipt_drill.sh          # 留库排查（打印库名）
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
DB="cp_m203_receipt_drill_$$"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/tmp}"
LOG="$TMP/m203_drill_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
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

printf '== M2-03 签收确认 → 在途转已出库 · 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 补齐迁移（模板落后于代码） ----------
echo "== 1) 补齐业务迁移 V0405…V0420（幂等，逐条 ON_ERROR_STOP）"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR"/V04{05,06,07,08,09,10,11,12,13,14,15,16,17,18,19,20}__*.up.sql 2>/dev/null | sort); do
    APPLIED=$((APPLIED+1))
    if ! psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$f" >"$LOG" 2>&1; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$(basename "$f")（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条业务迁移全部应用成功（V0420 自检通过 = 在途账表就位）"
fi
assert_eq "biz_inventory_in_transit" "$(psql_q "SELECT to_regclass('public.biz_inventory_in_transit')")" "在途账表存在"

# ---------- 2) 真库用例 ----------
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out
    out="$TMP/m203_$$_${module//[:\/]/_}.out"
    ( cd "$APP_DIR" && cargo test --lib "$module" -- --test-threads=1 ) >"$out" 2>&1
    local line passed failed
    line="$(grep -E '^test result: ' "$out" | head -1)"
    passed="$(printf '%s' "$line" | sed -n 's/.*ok\. \([0-9]*\) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -n 's/.* \([0-9]*\) failed.*/\1/p')"
    [ -z "${passed:-}" ] && passed=0
    [ -z "${failed:-}" ] && failed=0
    if [ "$failed" -eq 0 ]; then
        assert_ge "$min" "$passed" "$label：$module 全绿（$passed 条）"
    else
        bad "$label：$module 有 $failed 条失败（日志 $out）"
    fi
}

echo "== 2) 本卡真库用例（串行；ERP_DB_URL 指向演练库）"
run_group "business::erp::shipment::shipment_receipt_tests" 10 "本卡主证据（签收确认 + 作废释放在途）"

# ---------- 3) 落库形态复核（不依赖 Rust 断言） ----------
# ⚠ 取样时机：就在本卡用例跑完之后、其它模块的 `cleanup()` 之前 ——
#    每个真库模块的 `setup()` 都会先清自己的夹具，跨界跑完再查就查不到本卡的数据了。
echo "== 3) 落库形态复核（SQL 直查，取样于本卡用例刚跑完）"

# 3.1 在途行的生成列恒等式：qty_in_transit = shipped_qty - received_qty
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit WHERE qty_in_transit <> shipped_qty - received_qty")" \
    "在途量 = 发货量 − 累计签收（生成列恒等，异常行 0）"

# 3.2 对账式（M2-05 预告）：全局 sum(shipped) = sum(received) + sum(qty_in_transit)
assert_eq "0" "$(psql_q "SELECT count(*) FROM (SELECT sum(shipped_qty) AS s, sum(received_qty) + sum(qty_in_transit) AS r FROM biz_inventory_in_transit) t WHERE t.s <> t.r")" \
    "对账恒等式 sum(发货) = sum(已签收) + sum(在途)"

# 3.3 状态×数量咬合 + RECEIVED 必须有签收时间
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit WHERE (status='IN_TRANSIT' AND received_qty >= shipped_qty) OR (status='RECEIVED' AND received_qty <> shipped_qty)")" \
    "在途行状态×数量咬合（IN_TRANSIT 有余量 / RECEIVED 已签满）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit WHERE status='RECEIVED' AND received_at IS NULL")" \
    "RECEIVED 必须有 received_at（V0420 的 CHECK 在库层兜住）"

# 3.4 ★ 本卡的核心不变量①：**不许**「出库单已过账，但该发货单还有未签收在途」
#     （= 扣了库存却把在途挂着 ⇒ 可用量永久少一批货）
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id JOIN biz_inventory_in_transit t ON t.shipment_id = s.id WHERE o.source_type='SALES' AND o.status='POSTED' AND t.status='IN_TRANSIT'")" \
    "出库单已过账 ⇒ 该发货单没有未签收在途（扣库存与清在途同事务）"

# 3.5 ★ 核心不变量②：**不许**「出库单还在待确认，但该发货单在途已清零」
#     （= 清了在途却没扣库存 ⇒ 库存虚高）
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id WHERE o.source_type='SALES' AND o.status='PENDING_POST' AND NOT EXISTS (SELECT 1 FROM biz_inventory_in_transit t WHERE t.shipment_id = s.id AND t.status='IN_TRANSIT') AND EXISTS (SELECT 1 FROM biz_inventory_in_transit t WHERE t.shipment_id = s.id)")" \
    "出库单待确认且有在途行 ⇒ 必有一行未签收（清在途与扣库存同事务）"

# 3.6 正向计数：本卡用例确实把「待确认 → 已过账」这条路走通了
assert_ge 1 "$(psql_q "SELECT count(DISTINCT s.id) FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id JOIN biz_inventory_in_transit t ON t.shipment_id = s.id WHERE o.source_type='SALES' AND o.status='POSTED' AND t.status='RECEIVED'")" \
    "至少 1 张发货单：签收后出库单已过账 + 在途 RECEIVED"
assert_ge 1 "$(psql_q "SELECT count(*) FROM biz_voucher v JOIN biz_stock_out o ON o.id = v.source_id JOIN biz_shipment s ON s.id = o.source_id WHERE v.source_type='STOCK_OUT' AND s.warehouse_id IN (SELECT id FROM biz_warehouse WHERE warehouse_code='SO10R-WH')")" \
    "签收确认生成了成本结转凭证（本卡夹具仓库）"
assert_ge 1 "$(psql_q "SELECT count(*) FROM biz_receivable r JOIN biz_stock_out o ON o.id = r.source_id JOIN biz_shipment s ON s.id = o.source_id WHERE r.source_type='SALES_OUT' AND s.warehouse_id IN (SELECT id FROM biz_warehouse WHERE warehouse_code='SO10R-WH')")" \
    "签收确认挂了应收（本卡夹具仓库）"

# 3.7 已签收的单不许再有成本/库位账与流水脱节：POSTED 的销售出库单流水净额 = −总数量
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id WHERE o.source_type='SALES' AND o.status='POSTED' AND s.warehouse_id IN (SELECT id FROM biz_warehouse WHERE warehouse_code='SO10R-WH') AND (SELECT COALESCE(SUM(CASE WHEN t.txn_type='OUT' THEN -t.qty ELSE t.qty END),0) FROM biz_inventory_txn t WHERE t.doc_id = o.id) <> -o.total_qty")" \
    "已过账的销售出库单：OUT 流水净额 = −出库总数量"

# ---------- 3b) 作废释放在途（M2-FIX t_f123b57f）----------
# 取样时机说明：3.4/3.5/3.6 的正向计数依赖「签收确认」那批夹具，而每个真库用例的
# `setup()` 都会先清自己的夹具 —— 所以作废口径要单独再跑一遍作废用例再取样，
# 这样库里一定同时有「已作废的 SALES 出库单 + 它的在途行」可查（不是空表假绿）。
echo "== 3b) 作废释放在途：单独再跑一遍作废用例后取样（M2-FIX）"
run_group "business::erp::shipment::shipment_receipt_tests::cancel_pending_stock_out_releases_transit" 1 "作废用例（取样前最后一跑）"

# 3b.1 ★ 核心不变量③：**不许**「出库单已作废（SALES），但该发货单还有未签收在途」
#      （= 作废不释放在途 ⇒ 可用量（账面 − 在途）被永久占住，只能改库恢复）
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id JOIN biz_inventory_in_transit t ON t.shipment_id = s.id WHERE o.source_type='SALES' AND o.status='CANCELLED' AND t.status='IN_TRANSIT'")" \
    "已作废的销售出库单 ⇒ 该发货单没有未签收在途（作废与释放在途同事务）"

# 3b.2 该发货单的 Σ(未签收在途) 必须是 0（换个写法再查一遍：单据视角）
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_out o WHERE o.source_type='SALES' AND o.status='CANCELLED' AND COALESCE((SELECT SUM(t.qty_in_transit) FROM biz_inventory_in_transit t WHERE t.shipment_id = o.source_id AND t.status='IN_TRANSIT'), 0) <> 0")" \
    "已作废的销售出库单：其发货单未签收在途合计为 0（可用量已恢复）"

# 3b.3 正向计数（防空表假绿）：确实有「作废 ⇒ 在途行 CANCELLED 且带作废原因」的样本
assert_ge 1 "$(psql_q "SELECT count(*) FROM biz_stock_out o JOIN biz_shipment s ON s.id = o.source_id JOIN biz_inventory_in_transit t ON t.shipment_id = s.id WHERE o.source_type='SALES' AND o.status='CANCELLED' AND t.status='CANCELLED' AND COALESCE(t.remark,'') <> ''")" \
    "至少 1 张已作废的销售出库单：在途行 CANCELLED 且备注写了作废原因"

# 3b.4 作废不动数量：作废行的 shipped/received 必须原样（「发过货」是事实，审计要能回溯）
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit t WHERE t.status='CANCELLED' AND (t.shipped_qty <= 0 OR t.received_qty <> 0)")" \
    "作废行数量列原样（shipped > 0 / received = 0，只改状态）"


echo "== 4) 回归组：上游 / 上下游口径未被本卡破坏（串行）"
run_group "business::erp::shipment::shipment_posting" 11 "上游：发货过账仍在途 + 出库待确认"
run_group "business::erp::shipment::shipment_transit" 4 "在途账写入口"
run_group "business::erp::stockout" 60 "出库过账内核（⑤c 在途清零收口所在）"
run_group "business::erp::receivable::receivable_bridge" 13 "下游：签收确认才挂应收"

echo
printf '== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
    echo "FAILED"
    exit 1
fi
echo "ALL PASS"
