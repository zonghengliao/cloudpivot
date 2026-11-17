#!/usr/bin/env bash
# ============================================================================
# M3-13 客户信用「并发场景单测（并发下单 / 部分收款）+ 竞态」· 真库演练
#
# 卡：t_5fc52acf（M3-13【M3·销售】客户信用：单测（并发下单、部分收款））
# 上游：t_464b7265（M3-09 策略）/ t_8729b656（M3-10 实时占用）/
#       t_8a08ce48（M3-11 超信用拦截）/ t_f4f53ec4（M3-12 核销即释放）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 补模板落后的业务迁移 V0405…V0437（本卡**不加迁移**：无 DDL，下一个可用号仍 V0438）
#   3. 静态断言：并发用例的「防假绿」结构还在 ——
#      · 7 条用例全部用 multi_thread 运行时（current_thread 下 tokio::spawn 不会真并行）；
#      · 每个并发段之前都调 `warm_pool`（冷池建连握手会把并发错开成串行 = 竞态分支成死代码）；
#      · 用例 ① 里保留「3 轮内必须命中竞态」的护栏；
#      · 台账链断言 `assert_ledger_chain` 仍被调用（CAS 串行化的直接证据）；
#      · 没有临时探针残留（TEMP-PROBE）。
#   4. 死权限码常驻闸 `perm_menu_gate.py`（本卡不造新码，必须仍为 0）
#   5. 真库用例（--test-threads=1），**SKIP=0** 逐组断言：
#        · credit_concurrency_tests（本卡 7 条：并发提交 / 并发审批×2 / 同单重放 /
#          4 张收款并发 / 超余额一胜一拒 / 占用与释放并发）
#        · credit_release_tests（M3-12 的 7 条：部分 / 收清 / 调大 / 调小 / 重放 / 回滚 / 退款）
#        · credit 域整组 + receipt 域整组 + receipt_refund 域整组（不许被本卡打破）
#   6. 收尾断言：M313- 夹具 0 残留、**S9 对拍 0 漂移**（用例清场后缓存列仍等于公式）
#   7. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m313_credit_concurrency_drill.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m313_credit_concurrency_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m313_credit_concurrency_drill.sh          # 留库排查
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
APP_DIR="$ROOT/backend/app"
# 路径写**字面全路径**：`scripts/lint/path_refs_gate.py` 只认能直接解析的路径。
TESTS="$ROOT/backend/app/src/business/erp/credit_concurrency_tests.rs"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${DB:-cp_m313_concurrency_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
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
}
trap cleanup EXIT

printf '== M3-13 客户信用并发单测 · 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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
    if ! psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q \
            -f "$MIG_DIR/$f" >"$TMP/m313_mig_$$.log" 2>&1; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$TMP/m313_mig_$$.log" | tr '\n' ' ')）"
    fi
done
[ "$MIG_FAIL" -eq 0 ] && ok "$APPLIED 条业务迁移全部应用成功"
assert_eq "2" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_customer' AND column_name IN ('credit_policy','credit_used')")" \
    "biz_customer 的 credit_policy / credit_used 都在（M3-09 + SO-38 的列）"
if ls "$MIG_DIR" | grep -qE '^V0438__'; then
    info "已存在 V0438（别的卡落的），本卡无新增迁移"
else
    ok "本卡未新增迁移（下一个可用号 V0438 未被本卡占用）"
fi

# ---------- 2) 静态断言：并发用例的防假绿结构 ----------
echo "== 2) 静态断言：并发用例结构（7 条 / 预热 / 竞态护栏 / 台账链）"
assert_eq "7" "$(grep -c 'flavor = "multi_thread"' "$TESTS")" \
    "7 条用例全部用 multi_thread 运行时（current_thread 下 spawn 不会真并行）"
assert_ge 8 "$(grep -c 'warm_pool(' "$TESTS")" \
    "每个并发段之前都预热连接池（函数定义 + 7 处调用 ≥ 8 次）"
assert_eq "1" "$(grep -c '3 轮并发提交都没命中竞态' "$TESTS")" \
    "保留「竞态必须命中」的护栏（竞态分支不许变成死代码）"
assert_eq "1" "$(grep -c '^fn assert_ledger_chain(' "$TESTS")" \
    "台账链断言只有一份实现"
assert_ge 5 "$(grep -c 'assert_ledger_chain(&rows' "$TESTS")" \
    "台账链断言被 5 条用例复用（③ 是「零占用 ⇒ 台账必须为空」，不用链断言）"
assert_eq "0" "$(grep -c 'TEMP-PROBE' "$TESTS")" \
    "没有临时探针残留（TEMP-PROBE）"
assert_ge 8 "$(grep -c 'assert_converged(customer' "$TESTS")" \
    "缓存列 == 公式（S9 同源）的断言在每个并发场景后都跑"

# ---------- 3) 死权限码常驻闸（本卡不造新码） ----------
echo "== 3) perm_menu_gate.py（注解码 − 菜单码 必须为 0）"
if [ -f "$ROOT/scripts/verify/perm_menu_gate.py" ]; then
    if NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT="$PG_PORT" NIK_DATABASE_USER="$PG_USER" \
       NIK_DATABASE_NAME="$DB" NIK_DATABASE_PASSWORD="$PGPASSWORD" \
       python3 "$ROOT/scripts/verify/perm_menu_gate.py" >"$TMP/m313_perm_gate_$$.log" 2>&1; then
        ok "死权限码闸通过（$(grep -c . "$TMP/m313_perm_gate_$$.log") 行输出）"
    else
        bad "死权限码闸红：$(tail -3 "$TMP/m313_perm_gate_$$.log" | tr '\n' ' ')"
    fi
else
    info "perm_menu_gate.py 不存在，跳过"
fi

# ---------- 4) 真库用例 ----------
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
echo "== 4) 真库用例（--test-threads=1）"

run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m313_$$_${module//[:/]/_}.out"
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

run_group "business::erp::credit_concurrency_tests" 7 "本卡并发用例（提交 / 审批 / 部分收款并发 / 占用释放交错）"
run_group "business::erp::credit_release_tests" 7 "M3-12 回归（核销即释放六口径 + 回执契约）"
run_group "business::erp::credit" 30 "信用域整组（三释放出口 + 策略三态 + 实时快照 + 审批评估）"
run_group "business::erp::receipt" 35 "收款域整组（登记 / 核销 / 状态机 / 回调）"
run_group "business::erp::receipt_refund" 24 "退款域整组（冲减 / 反向边）"

# ---------- 5) 收尾断言 ----------
echo "== 5) 收尾：夹具 0 残留 + S9 对拍 0 漂移"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE so_no LIKE 'SO-M313-%'")" \
    "biz_sales_order 无 M313 夹具订单"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_receivable WHERE ar_no LIKE 'AR-M313-%'")" \
    "biz_receivable 无 M313 夹具应收"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_receipt WHERE customer_id IN (SELECT id FROM biz_customer WHERE code='M313-CUST')")" \
    "biz_receipt 无 M313 夹具收款"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_credit_log WHERE customer_id IN (SELECT id FROM biz_customer WHERE code='M313-CUST')")" \
    "信用台账无 M313 夹具行"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_status_log WHERE entity_id IN (SELECT id FROM biz_sales_order WHERE so_no LIKE 'SO-M313-%')")" \
    "销售订单无 M313 孤儿状态日志（M3-08 的口径：entity_type 落 SALE_ORDER）"
# S9 同源对拍（与 `credit::usage_of` / `erp_invariants.py S9` 逐字同式）：
# 用例收尾会把订单 / 应收删干净并把 credit_used 归 0，所以「缓存列 == 公式」必须仍成立。
DRIFT="$(psql_q "
    with occ as (select customer_id, sum(total_with_tax) s from biz_sales_order
                  where deleted_at is null and status in ('APPROVED','PARTIAL_OUT','OUTBOUND') group by 1),
         rec as (select customer_id, sum(received_amount) s from biz_receivable
                  where deleted_at is null group by 1),
         ret as (select customer_id, sum(total_with_tax) s from biz_sales_return
                  where deleted_at is null and status = 'POSTED' group by 1)
    select count(*) from biz_customer c
      left join occ on occ.customer_id = c.id
      left join rec on rec.customer_id = c.id
      left join ret on ret.customer_id = c.id
     where c.deleted_at is null and c.code like 'M313-%'
       and abs(coalesce(c.credit_used,0)
               - greatest(0, coalesce(occ.s,0) - coalesce(rec.s,0) - coalesce(ret.s,0))) > 0.01")"
assert_eq "0" "$DRIFT" "S9 对拍：M313 客户 credit_used 与公式无漂移（容差 0.01）"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
