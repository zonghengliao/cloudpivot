#!/usr/bin/env bash
# ============================================================================
# M4-13 供应商评估「打分 + 评级（A/B/C/D）」· 演练
#
# 卡：t_252f2eaf（M4-13【M4·采购】供应商评估：打分 + 评级（A/B/C/D））
#
# 这一卡**只交数据层**（评级阈值 + 算分 + 打分落库 + 重评级 + 交期参考分），
# 没有新迁移、零路由、零权限码、零菜单、零前端改动。脚本验的正是这些：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. 把演练库推到当前仓库版本（业务迁移 > V0404 全部应用，含 M4-12 的 V0454）
#   3. 本卡的 Rust 用例：6 条纯逻辑 + 10 条真库（含边界 8 点、regrade、交期参考分）
#      —— 要求 **0 failed + 0 SKIP + 用例数对得上**（SKIP 就是假绿）
#   4. 回归：M4-12 表结构用例（12 条）不许被我改动打红
#   5. psql 侧独立断言：等级值域仍是 NULL/A/B/C/D、夹具 0 残留、
#      **没有新迁移 / 没有新菜单 / 没有新权限码**（本卡不夹带）
#   6. 门禁：permission_audit_tests + menu_invariants.sh（本卡零新增，跑一遍证明没破）
#
# 用法：
#   bash scripts/verify/m413_supplier_evaluation_grading_drill.sh                  # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m413_supplier_evaluation_grading_drill.sh  # 在 145 上跑
#   KEEP_DB=1 bash scripts/verify/m413_supplier_evaluation_grading_drill.sh        # 留库排查
#   M413_BAD_URL=1 bash scripts/verify/m413_supplier_evaluation_grading_drill.sh   # SKIP 守卫自检（期望报红）
#   SKIP_GATES=1 bash scripts/verify/m413_supplier_evaluation_grading_drill.sh     # 跳过门禁（只验业务）
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名含 drill；脚本自己 DROP；
# 连接串密码一律复用 `$PGPASSWORD`（**不要**在 URL 里写占位符 —— 那样真库用例连不上、
# 打 SKIP 还记 ok，是假绿）。
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
DB="${M413_DB:-cp_m413_sev_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_GATES="${SKIP_GATES:-0}"
TMP="${TMPDIR:-/root/.hermes/profiles/backend-developer/cache/scratch}"
mkdir -p "$TMP"
LOG="$TMP/m413_drill_$$.log"
TEST_LOG="$TMP/m413_tests_$$.log"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

case "$DB" in
    *drill*|*e2e*|*test*) : ;;
    *) printf '库名 %s 不含 drill/e2e/test，拒绝在它上面跑（rc=2）\n' "$DB"; exit 2 ;;
esac

OWN_DB=1
if [ -n "${M413_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }

assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}

rejects_c() { # sql 约束名 说明
    local out
    out="$(psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -q -X -c "$1" 2>&1)"
    if ! printf '%s' "$out" | grep -q 'ERROR'; then
        bad "$2 —— 库层居然接受了（约束失效）"
        return
    fi
    case "$out" in
        *"invalid input syntax"*|*"does not exist"*|*"cannot cast"*)
            bad "$2 —— 被拒了但**不是约束**（SQL 自己写错：$(printf '%s' "$out" | head -1)）"
            ;;
        *)
            if printf '%s' "$out" | grep -q "\"$3\""; then
                ok "$2（库层拒了：$3）"
            else
                bad "$2 —— 报错但不是 $3 拦的：$(printf '%s' "$out" | head -1)"
            fi
            ;;
    esac
}

cleanup() {
    if [ "$OWN_DB" = "0" ] || [ "$KEEP_DB" = "1" ]; then
        printf '\n  （库保留 → %s；不删）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG" "$TEST_LOG"
}
trap cleanup EXIT

printf '== M4-13 供应商评估打分 + 评级演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"

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

# ---------- 1) 推到仓库当前版本 ----------
echo "== 1) 应用业务迁移 V0405…（含 M4-12 的 V0454）"
BASE_VERSION="$(psql_q "SELECT COALESCE(max(version::int),0) FROM schema_migrations WHERE kind='versioned'")"
if ! printf '%s' "$BASE_VERSION" | grep -qE '^[0-9]+$'; then
    bad "读不出模板库的迁移版本：$BASE_VERSION"
    exit 2
fi
info "模板库版本 = V$BASE_VERSION"
MIG_FAIL=0
APPLIED=0
for f in $(ls "$MIG_DIR" | grep -E '^V[0-9]{4}__.*\.up\.sql$' | sort); do
    num="${f:1:4}"
    if [ "$((10#$num))" -le "$BASE_VERSION" ]; then continue; fi
    APPLIED=$((APPLIED+1))
    if ! psql_file "$MIG_DIR/$f"; then
        MIG_FAIL=$((MIG_FAIL+1))
        bad "迁移失败：$f（tail: $(tail -2 "$LOG" | tr '\n' ' ')）"
    fi
done
if [ "$MIG_FAIL" -eq 0 ]; then
    ok "$APPLIED 条迁移全部应用成功（演练库 = 仓库当前版本）"
fi
assert_eq "1" \
    "$(psql_q "SELECT (to_regclass('public.biz_supplier_evaluation') IS NOT NULL)::int")" \
    "评估表存在（V0454 已落地）"

# ---------- 2) 本卡 Rust 用例 ----------
echo "== 2) 本卡 Rust 用例（6 纯逻辑 + 10 真库）"
if [ -n "${M413_BAD_URL:-}" ]; then
    export ERP_DB_URL="postgres://$PG_USER:${PGPASSWORD}@127.0.0.1:1/nonexistent"
    info "M413_BAD_URL 已设：故意给一个连不上的库（验 SKIP 守卫）"
else
    export ERP_DB_URL="postgres://$PG_USER:${PGPASSWORD}@$PG_HOST:$PG_PORT/$DB"
fi
run_tests() { # 过滤串 说明 期望最少通过数
    local filter="$1" label="$2" want="$3"
    ( cd "$BACKEND" && cargo test -p cloudpivot --lib "$filter" -- --test-threads=1 ) >"$TEST_LOG" 2>&1
    local rc=$?
    local line
    line="$(grep -E '^test result:' "$TEST_LOG" | tail -1)"
    local passed failed
    passed="$(printf '%s' "$line" | sed -nE 's/.* ([0-9]+) passed.*/\1/p')"
    failed="$(printf '%s' "$line" | sed -nE 's/.* ([0-9]+) failed.*/\1/p')"
    passed="${passed:-0}"; failed="${failed:-0}"
    if [ "$rc" -ne 0 ]; then
        bad "$label：cargo test rc=$rc（$(grep -E '^(error|test .* FAILED|thread)' "$TEST_LOG" | head -2 | tr '\n' ' ')）"
        return 1
    fi
    assert_eq "0" "$failed" "$label 失败数"
    if [ "$passed" -ge "$want" ]; then
        ok "$label 通过 $passed 条（要求 >= $want）"
    else
        bad "$label 只通过 $passed 条（要求 >= $want）—— 用例被 SKIP 或没编译进去"
    fi
    local skipped
    skipped="$(grep -c 'SKIP:' "$TEST_LOG")"
    assert_eq "0" "$skipped" "$label SKIP 行数（真库用例不许默默跳过）"
    # 证据：把这一轮的原始输出追加进证据文件（设了 M413_EVIDENCE 才写）
    if [ -n "${M413_EVIDENCE:-}" ]; then
        {
            printf '\n===== %s（filter=%s）=====\n' "$label" "$filter"
            cat "$TEST_LOG"
        } >>"$M413_EVIDENCE"
    fi
}

run_tests "business::erp::purchase::supplier_evaluation_grading" "打分+评级（M4-13）" 16
if [ -z "${M413_BAD_URL:-}" ]; then
    run_tests "business::erp::purchase::supplier_evaluation_db_tests" "表结构回归（M4-12）" 12
fi

if [ -n "${M413_BAD_URL:-}" ]; then
    echo "== M413_BAD_URL 模式：上面应当出现 FAIL（SKIP 守卫有效）=="
    if [ "$FAIL" -gt 0 ]; then printf '  rc=1（失败 %d 条，符合预期）\n' "$FAIL"; exit 1; fi
    printf '  居然全绿 —— SKIP 守卫失效\n'; exit 0
fi

# ---------- 3) psql 侧独立断言 ----------
echo "== 3) 库层不变量（psql 侧，独立于 Rust）"
assert_eq "25" "$(psql_q "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='biz_supplier_evaluation'")" \
    "评估表仍是 25 列（本卡没有加列）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM pg_constraint WHERE conrelid='biz_supplier_evaluation'::regclass AND conname='ck_biz_sev_grade'")" \
    "等级值域 CHECK 还在（本卡不改 DDL）"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, eval_date, grade) VALUES ('SEV-19990101-913001',gen_random_uuid(),'2028-01-01','2028-03-31','2028-04-01','E')" \
    "等级域外的 E 仍被拒（分级值域没被本卡放宽）" "ck_biz_sev_grade"
rejects_c "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, eval_date, quality_weight, delivery_weight, price_weight, service_weight) VALUES ('SEV-19990101-913002',gen_random_uuid(),'2028-01-01','2028-03-31','2028-04-01',0.4,0.3,0.2,0.2)" \
    "权重和 1.1 仍被拒（算分前提没被放松）" "ck_biz_sev_weights"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_supplier_evaluation WHERE supplier_name LIKE 'M413-%' OR remark LIKE 'M413-%'")" \
    "本卡评估夹具 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'M413-%'")" \
    "本卡入库夹具 0 残留"
assert_eq "0" "$(psql_q "SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'M413-%'")" \
    "本卡采购单夹具 0 残留"

echo "== 4) 不夹带：零迁移 / 零菜单 / 零权限码 / 零路由"
assert_eq "0" "$(git -C "$ROOT" status --porcelain backend/app/migrations | wc -l | tr -d ' ')" \
    "本卡没有新增/修改迁移（V0454 是 M4-12 的）"
assert_eq "0" "$(psql_q "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND (auth_code LIKE '%supplier-evaluation%' OR auth_code LIKE '%evaluation%' OR path LIKE '%supplier-evaluation%' OR path LIKE '%evaluation%')")" \
    "菜单里没有供应商评估相关行（页面属 M4-16）"
assert_eq "0" "$(grep -c '#\[nik::' "$ROOT/backend/app/src/business/erp/purchase/supplier_evaluation_grading.rs" || true)" \
    "新 model 文件里没有任何路由宏（本卡零路由）"
assert_eq "0" "$(grep -c 'nik::permission' "$ROOT/backend/app/src/business/erp/purchase/supplier_evaluation_grading.rs" || true)" \
    "新 model 文件里没有任何权限码声明"

# ---------- 4) 门禁 ----------
if [ "$SKIP_GATES" = "1" ]; then
    info "SKIP_GATES=1 → 跳过 permission_audit_tests / menu_invariants.sh"
else
    echo "== 5) 门禁（本卡零新增，跑一遍证明没破）"
    run_tests "system::menu::permission_audit_tests" "权限审计" 8
    MENU_OUT="$(cd "$ROOT" && PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" bash scripts/verify/menu_invariants.sh "$DB" 2>&1)"
    MENU_RC=$?
    if [ "$MENU_RC" -eq 0 ]; then
        ok "menu_invariants.sh 8 条全绿（$(printf '%s' "$MENU_OUT" | grep -cE '^ *(✓|PASS)' || true) 条输出行）"
    else
        bad "menu_invariants.sh rc=$MENU_RC：$(printf '%s' "$MENU_OUT" | grep -E '✗|FAIL|违规' | head -3 | tr '\n' ' ')"
    fi
fi

# ---------- 5) 收尾 ----------
echo
if [ "$FAIL" -eq 0 ]; then
    printf '== 结果：断言通过 %d 条 / 失败 0 条 · rc=0 ==\n' "$PASS"
    exit 0
fi
printf '== 结果：断言通过 %d 条 / 失败 %d 条 · rc=1 ==\n' "$PASS" "$FAIL"
exit 1
