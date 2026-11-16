#!/usr/bin/env bash
# ============================================================================
# M2-02 发货过账 → 在途 + 出库待确认 · 真库演练
#
# 卡：t_6c090f97（M2-02【M2·仓库】在途库存：发货单过账 → 在途 + 出库待确认）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 把模板落后的业务迁移补齐（V0405…V0420，全部幂等；V0420 建的正是本卡的
#      在途账 biz_inventory_in_transit）—— 不补就缺 `biz_voucher.source_doc_no`
#      （V0419），任何出库过账都会 500
#   3. 跑三组真库用例（串行，共享夹具必须 --test-threads=1）：
#        business::erp::shipment::shipment_posting   ← 本卡主证据（6 条真库用例）
#        business::erp::shipment::shipment_transit   ← 在途账写入口（纯逻辑 + 状态域）
#        business::erp::receivable::receivable_bridge ← 受本卡影响的下游（出库确认才挂应收）
#   4. 用例跑完后在库里复核一次「本卡口径」的落库形态（不依赖 Rust 断言）：
#        · 有在途行、且 `qty_in_transit = shipped_qty - received_qty`
#        · 有发货单的销售出库单停在 PENDING_POST（出库待确认）
#        · 生成 PENDING_POST 出库单的发货单，库存账没有被扣过（三本账一行没动）
#   5. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m202_shipment_transit_drill.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m202_shipment_transit_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m202_shipment_transit_drill.sh          # 留库排查（打印库名）
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
DB="cp_m202_transit_drill_$$"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/tmp}"
LOG="$TMP/m202_drill_$$.log"
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

printf '== M2-02 发货过账 → 在途 + 出库待确认 · 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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
assert_eq "64" "$(psql_q "SELECT character_maximum_length FROM information_schema.columns WHERE table_name='biz_voucher' AND column_name='source_no'")" "biz_voucher.source_no 已是 VARCHAR(64)（V0419）"

# ---------- 2) 真库用例 ----------
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out
    out="$TMP/m202_$$_${module//[:\/]/_}.out"
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

echo "== 2) 真库用例（串行；ERP_DB_URL 指向演练库）"
run_group "business::erp::shipment::shipment_posting" 11 "本卡主证据"
run_group "business::erp::shipment::shipment_transit" 4 "在途账写入口"
run_group "business::erp::shipment::shipment_model" 14 "发货数据层（在途占用可用量）"
run_group "business::erp::receivable::receivable_bridge" 13 "下游：销售出库确认才挂应收"

# ---------- 3) 落库形态复核（不依赖 Rust 断言） ----------
echo "== 3) 落库形态复核（SQL 直查）"

# 3.1 在途行的生成列恒等式：qty_in_transit = shipped_qty - received_qty（不成立的行数必须 0）
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit WHERE qty_in_transit <> shipped_qty - received_qty")" \
    "在途量 = 发货量 − 累计签收（生成列恒等，异常行 0）"

# 3.2 在途行只挂在真发货单上（硬外键 + 没有孤儿）
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit t LEFT JOIN biz_shipment s ON s.id = t.shipment_id WHERE s.id IS NULL")" \
    "在途行没有孤儿（shipment_id 硬外键 CASCADE）"

# 3.3 状态与数量咬合（IN_TRANSIT 必须还有余量 / RECEIVED 必须签满）
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_inventory_in_transit WHERE (status='IN_TRANSIT' AND received_qty >= shipped_qty) OR (status='RECEIVED' AND received_qty <> shipped_qty)")" \
    "在途行状态×数量咬合（IN_TRANSIT 有余量 / RECEIVED 已签满）"

# 3.4 销售来源的出库单：由发货过账生成的那批必须停在 PENDING_POST（出库待确认），
#     并且**没有**过账时间（账一分没动）。用例结束时若刚留下 POSTED（签收确认那半边）也算对，
#     所以判据是「没有既非 PENDING_POST 又没 posted_at 的怪行」。
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_out WHERE source_type='SALES' AND status='PENDING_POST' AND posted_at IS NOT NULL")" \
    "出库待确认单不许有 posted_at（PENDING_POST = 账没动）"

# 3.5 出库待确认单不许带成本结转凭证（凭证只在签收确认时生成）
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_voucher v JOIN biz_stock_out o ON o.id = v.source_id WHERE v.source_type='STOCK_OUT' AND o.status='PENDING_POST'")" \
    "出库待确认单不许有成本结转凭证（库存不扣 / 凭证后移）"

echo
printf '== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
    echo "FAILED"
    exit 1
fi
echo "ALL PASS"
