#!/usr/bin/env bash
# ============================================================================
# M3-10 客户信用「下单实时占用 / 可用额度」· 真库演练
#
# 卡：t_8729b656（M3-10【M3·销售】客户信用：下单时实时占用（未清应收 + 本次订单）计算可用额度）
# 上游：t_464b7265（M3-09 超限策略，已把 V0437 坐实）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 补模板落后的业务迁移 V0405…V0437（本卡**不加迁移**：只读口径，无 DDL）
#   3. 静态断言：新路由与权限声明成对出现；复用的读码在 `sys_menu` 有承载行（不是哑弹）
#   4. 死权限码常驻闸 `perm_menu_gate.py`（`注解码 − 菜单码` 必须为 0）
#   5. 真库用例（--test-threads=1），**SKIP=0** 逐组断言（真库用例被打成 SKIP 就是假绿）：
#        · credit_handler 域（快照算术 / 入参闸 / 策略三态 / 边界 / 404·400）
#        · credit 域（占用·释放·重算 + 策略判定不被打破）
#        · customer 域（客户档案 CRUD 不被打破）
#        · sales 域（订单提交守卫三态 + 报价单 / 退货 / 审批桥）
#   6. 断言夹具清场：CR10- / SO10- / AR-SO10- / CR38- 与警示台账 0 残留
#   7. 打印 PASS/FAIL 汇总；任一不符退出码 1
#
# 用法：
#   bash scripts/verify/m310_credit_realtime_drill.sh                    # 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m310_credit_realtime_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m310_credit_realtime_drill.sh          # 留库排查
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
DB="cp_m310_credit_drill_$$"
KEEP_DB="${KEEP_DB:-0}"

TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m310_drill_$$.log"
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

printf '== M3-10 客户信用实时占用 / 可用额度 · 演练（PG %s:%s / 库 %s）==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

PSQL_MIG="psql -h $PG_HOST -p $PG_PORT -U $PG_USER -d $DB -v ON_ERROR_STOP=1 -q"

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
# 本卡依赖的两列必须在位（快照读它们）
assert_eq "2" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_customer' AND column_name IN ('credit_policy','credit_used')")" \
    "biz_customer 的 credit_policy / credit_used 都在（M3-09 + SO-38 的列）"

# ---------- 2) 静态断言：路由 / 权限成对 ----------
echo "== 2) 静态断言：新路由 + 复用读码（不造新码 = 不给权限树添哑弹）"
SRC="$ROOT/backend/app/src/business/erp/credit_handler.rs"
if [ -f "$SRC" ]; then
    ok "credit_handler.rs 存在"
else
    bad "credit_handler.rs 缺失"
fi
assert_eq "1" "$(grep -c 'path = "/business/erp/credit/{customer_id}"' "$SRC" 2>/dev/null)" \
    "permission 宏里的路由路径只有一处（重复定义 = 启动期路由冲突）"
assert_eq "1" "$(grep -c 'nik::get("/business/erp/credit/{customer_id}")' "$SRC" 2>/dev/null)" \
    "get 宏里的路由路径与 permission 宏逐字一致（写错一处 = 断言漂移）"
assert_eq "1" "$(grep -c 'permission = \"business:erp:customer:list\"' "$SRC" 2>/dev/null)" \
    "路由声明的权限码 = 客户读码（与 customer_handler::PERM_LIST 逐字一致，单测也钉住）"
assert_ge "1" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND auth_code='business:erp:customer:list'")" \
    "复用的读码在 sys_menu 有承载行（授得出去）"

# ---------- 3) 死权限码常驻闸 ----------
echo "== 3) perm_menu_gate.py（注解码 − 菜单码 必须为 0）"
if NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT="$PG_PORT" NIK_DATABASE_USER="$PG_USER" \
   NIK_DATABASE_NAME="$DB" NIK_DATABASE_PASSWORD="$PGPASSWORD" \
   python3 "$ROOT/scripts/verify/perm_menu_gate.py" >"$TMP/m310_perm_gate_$$.log" 2>&1; then
    ok "死权限码闸通过（$(grep -c . "$TMP/m310_perm_gate_$$.log") 行输出，见日志）"
else
    bad "死权限码闸红：$(tail -5 "$TMP/m310_perm_gate_$$.log" | tr '\n' ' ')"
fi
rm -f "$TMP/m310_perm_gate_$$.log"

# ---------- 4) 真库用例 ----------
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
export SYS_DB_URL="$ERP_DB_URL"
echo "== 4) 真库用例（--test-threads=1）"

run_group() { # 模块路径 最少用例数 说明
    local module="$1" min="$2" label="$3" out line passed failed skipped
    out="$TMP/m310_$$_${module//[:/]/_}.out"
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

run_group "business::erp::credit_handler" 9 "本卡快照域（实时占用 / 可用额度 / 入参闸 / 策略三态）"
run_group "business::erp::credit" 20 "信用域（占用·释放·重算 + 策略判定不被打破）"
run_group "business::erp::customer" 15 "客户档案域（CRUD 不被打破）"
run_group "business::erp::sales" 200 "销售域整组（订单提交守卫 + 报价单 / 退货 / 审批桥）"

# ---------- 5) 夹具清场 ----------
echo "== 5) 夹具清场（不允许把本卡夹具带出演练库）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE 'CR10-%'")" "biz_customer 无 CR10- 夹具客户（本卡）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_sales_order WHERE so_no LIKE 'SO10-%'")" "biz_sales_order 无 SO10- 夹具订单（本卡）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_receivable WHERE ar_no LIKE 'AR-SO10-%'")" "biz_receivable 无 AR-SO10- 夹具应收（本卡）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_customer WHERE code LIKE 'CR38-%'")" "biz_customer 无 CR38- 夹具客户（credit 域）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_credit_log l LEFT JOIN biz_customer c ON c.id = l.customer_id WHERE c.code LIKE 'CR10-%' OR c.code LIKE 'CR38-%'")" "警示台账无夹具客户残留行"

# ---------- 汇总 ----------
printf '\n== 汇总：PASS=%d FAIL=%d ==\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "ALL PASS"
    exit 0
fi
echo "有失败项，见上面的 FAIL 行"
exit 1
