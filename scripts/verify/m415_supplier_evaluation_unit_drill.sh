#!/usr/bin/env bash
# ============================================================================
# M4-15 供应商评估「单测」演练（含评级边界穷举 + 并发/跨期/大方量 + 变异反证）
#
# 卡：t_73b6adf8（M4-15【M4·采购】供应商评估：单测 · 含评级边界）
#
# 这一卡交的是**测试**（本卡零新迁移、零新接口、零前端改动），脚本验的是：
#   1. 从模板库克隆演练库 → 推到仓库当前版本（V0454 的评估表 + SEV 取号）
#   2. **假绿对照**：不连库时整组 12 条依旧 passed（真库用例提前 return），
#      但本卡正向标记只有 5 条（纯逻辑）—— 所以「passed 数」不能用来判真假绿
#   3. 真库：本卡 12 条标记全部出现、0 failed、0 SKIP
#   4. 回归：M4-12 表结构 + M4-13 打分评级 + 本卡 整组（supplier_evaluation）全绿
#      —— 本卡顺带修了 save_evaluation 的读回对拍（并发误报 400），这里要证明旧用例没被改坏
#   5. **变异反证（4 个）**：把判据改错 → 用例必须转红（且红的是对应用例）→ 恢复 → 转绿
#   6. 门禁：cargo fmt --check / clippy --lib / clippy --all-targets / P2-62 夹具门禁
#   7. 不夹带：本卡 0 个迁移文件、0 个前端文件；生产代码只动 save_evaluation 的读回对拍
#
# 用法（99 开发机连 145 的 PG）：
#   bash scripts/verify/m415_supplier_evaluation_unit_drill.sh
#   PG_HOST=127.0.0.1 bash scripts/verify/m415_supplier_evaluation_unit_drill.sh   # 在 145 上跑
#   KEEP_DB=1 …   留库排查；SKIP_GATES=1 …   跳过门禁（只验业务）
#
# 退出码：0 = 全过；1 = 有断言失败；2 = 环境问题（建库 / 迁移 / 编译）
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG_DIR="$ROOT/backend/app/migrations/business"
BACKEND="$ROOT/backend"
SUITE_SRC="backend/app/src/business/erp/purchase/supplier_evaluation_suite_tests.rs"
GRADING_SRC="backend/app/src/business/erp/purchase/supplier_evaluation_grading.rs"
MUT="$ROOT/scripts/verify/m415_mutations.py"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="${M415_DB:-cp_m415_ut_drill_$$}"
KEEP_DB="${KEEP_DB:-0}"
SKIP_GATES="${SKIP_GATES:-0}"
OUTDIR="${OUTDIR:-$ROOT/docs/evidence/2026-11-18-m415-supplier-evaluation-unit}"
TMP="${TMPDIR:-$ROOT/../.scratch}"
mkdir -p "$TMP" "$OUTDIR"
MUT_BAK="$TMP/m415-mutations"
LOG="$TMP/m415_drill_$$.log"

# 用例规模：本卡 12 条（5 纯逻辑 + 7 真库）；真库整组 = M4-12 + M4-13 + 本卡
SUITE_TESTS="${SUITE_TESTS:-12}"
PURE_TESTS="${PURE_TESTS:-5}"
GROUP_TESTS="${GROUP_TESTS:-53}"

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
if [ -n "${M415_DB:-}" ]; then OWN_DB=0; fi

psql_q()     { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
psql_admin() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }
psql_file()  { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" >"$LOG" 2>&1; }

assert_eq() {
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 [$1]，实际 [$2]）"; fi
}
assert_ge() {
    if [ "${1:-0}" -ge "${2:-0}" ] 2>/dev/null; then ok "$3（$1 ≥ $2）"; else bad "$3（期望 ≥ [$2]，实际 [$1]）"; fi
}

restore_all() {
    for key in A B C D; do python3 "$MUT" "$key" restore "$MUT_BAK" >/dev/null 2>&1; done
}

cleanup() {
    restore_all
    if [ "$OWN_DB" = "0" ] || [ "$KEEP_DB" = "1" ]; then
        printf '\n  （库保留 → %s；不删）\n' "$DB"
        return
    fi
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    rm -f "$LOG"
}
trap cleanup EXIT

result_field() { # $1=输出文件 $2=passed|failed|ignored（多测试目标时取最大值）
    grep -oE "test result: (ok|FAILED)\. [0-9]+ passed; [0-9]+ failed; [0-9]+ ignored" "$1" \
        | awk -v k="$2" '{for (i = 1; i <= NF; i++) if ($(i+1) == k";") {print $i; break}}' \
        | sort -n | tail -1
}

run_suite() { # $1=输出文件；其余透传给 cargo test（本卡 12 条）
    local out="$1"; shift
    (cd "$BACKEND" && "$@" cargo test -p cloudpivot --lib business::erp::purchase::supplier_evaluation_suite \
        -- --test-threads=1 --nocapture) >"$out" 2>&1
}

printf '== M4-15 供应商评估单测演练 · PG %s:%s / 库 %s ==\n' "$PG_HOST" "$PG_PORT" "$DB"
# 变异反证前先记下生产代码的基线指纹：活动期间绝不能悄悄改变它（只有变异恢复要还原到这里）
GRADING_SHA_BEFORE="$(sha256sum "$ROOT/$GRADING_SRC" | awk '{print $1}')"
MODEL_SHA_BEFORE="$(sha256sum "$ROOT/backend/app/src/business/erp/purchase/supplier_evaluation_model.rs" | awk '{print $1}')"

# ---------- 0) 模板与克隆 ----------
echo "== 0) 前置：模板库可克隆"
if ! psql_admin "SELECT 1 FROM pg_database WHERE datname = '$TEMPLATE_DB'" | grep -q 1; then
    bad "模板库 $TEMPLATE_DB 不存在"; exit 2
fi
ok "模板库 $TEMPLATE_DB 存在"
if [ "$OWN_DB" = "1" ]; then
    psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
    if psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE_DB" >/dev/null 2>&1; then
        ok "从模板克隆演练库 $DB"
    else
        bad "克隆演练库失败"; exit 2
    fi
else
    ok "使用调用方给的演练库 $DB（只跑不清）"
fi

# ---------- 1) 推到仓库当前版本 ----------
echo "== 1) 应用业务迁移（模板 → 仓库当前版本）"
BASE_VERSION="$(psql_q "SELECT COALESCE(max(version::int),0) FROM schema_migrations WHERE kind='versioned'")"
if ! printf '%s' "$BASE_VERSION" | grep -qE '^[0-9]+$'; then
    bad "读不出模板库的迁移版本：$BASE_VERSION"; exit 2
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
assert_eq "0" "$MIG_FAIL" "迁移全部成功（应用 $APPLIED 个）"
assert_eq "1" "$(psql_q "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_name='biz_supplier_evaluation'")" 'M4-12 的评估表在位'
assert_eq "1" "$(psql_q "SELECT count(*) FROM biz_seq WHERE seq_type='SEV' AND deleted_at IS NULL")" 'SEV 取号行在位'

# ---------- 2) 不连库：假绿对照 ----------
echo '==> 2. 不连库：整组仍 passed，但本卡标记只有纯逻辑那 5 条（假绿对照）'
run_suite "$OUTDIR/01_suite_without_db.out" env -u ERP_DB_URL
RC=$?
NO_DB_PASSED=$(result_field "$OUTDIR/01_suite_without_db.out" passed)
NO_DB_FAILED=$(result_field "$OUTDIR/01_suite_without_db.out" failed)
NO_DB_MARKS=$(grep -c '\[M4-15\]' "$OUTDIR/01_suite_without_db.out" || true)
assert_eq "0" "$RC" '不连库时返回 0（跳过的用例不算失败）'
assert_eq "0" "$NO_DB_FAILED" '不连库时 0 failed'
assert_eq "$SUITE_TESTS" "$NO_DB_PASSED" "不连库时 passed 仍是 $SUITE_TESTS 条（passed 数不能判真假绿）"
assert_eq "$PURE_TESTS" "$NO_DB_MARKS" "不连库时只有 $PURE_TESTS 条纯逻辑真跑了（真库用例提前 return）"

# ---------- 3) 真库：本卡 12 条全跑 ----------
echo '==> 3. 真库：本卡 12 条标记全部出现，0 failed / 0 SKIP'
export ERP_DB_URL="postgres://$PG_USER:$PGPASSWORD@$PG_HOST:$PG_PORT/$DB"
run_suite "$OUTDIR/02_suite_with_db.out"
RC=$?
DB_PASSED=$(result_field "$OUTDIR/02_suite_with_db.out" passed)
DB_FAILED=$(result_field "$OUTDIR/02_suite_with_db.out" failed)
DB_SKIPS=$(grep -c 'SKIP: ' "$OUTDIR/02_suite_with_db.out" || true)
DB_MARKS=$(grep -c '\[M4-15\]' "$OUTDIR/02_suite_with_db.out" || true)
DB_RAN=$(grep -cE "^test business::erp::purchase::supplier_evaluation_suite_tests::" "$OUTDIR/02_suite_with_db.out" || true)
DB_RED=$(grep -cE "^test .*suite_tests::.*FAILED" "$OUTDIR/02_suite_with_db.out" || true)
assert_eq "0" "$RC" '真库本卡返回 0'
assert_eq "0" "$DB_FAILED" '真库本卡 0 failed'
assert_eq "0" "$DB_SKIPS" '真库本卡 SKIP=0（一条都没跳过）'
assert_eq "$SUITE_TESTS" "$DB_PASSED" "真库本卡 passed = $SUITE_TESTS"
assert_eq "$SUITE_TESTS" "$DB_RAN" "本卡 $SUITE_TESTS 条逐条执行"
assert_eq "$SUITE_TESTS" "$DB_MARKS" "本卡 $SUITE_TESTS 条正向标记全部出现"
assert_eq "0" "$DB_RED" '本卡 0 条 FAILED'

# ---------- 4) 回归：M4-12 + M4-13 + 本卡 ----------
echo '==> 4. 回归：整组 supplier_evaluation（M4-12 表结构 + M4-13 打分评级 + 本卡）'
(cd "$BACKEND" && cargo test -p cloudpivot --lib business::erp::purchase::supplier_evaluation \
    -- --test-threads=1) >"$OUTDIR/03_group_with_db.out" 2>&1
RC=$?
GROUP_PASSED=$(result_field "$OUTDIR/03_group_with_db.out" passed)
GROUP_FAILED=$(result_field "$OUTDIR/03_group_with_db.out" failed)
GROUP_SKIPS=$(grep -c 'SKIP: ' "$OUTDIR/03_group_with_db.out" || true)
assert_eq "0" "$RC" '整组返回 0'
assert_eq "0" "$GROUP_FAILED" '整组 0 failed（本卡改的读回对拍没弄坏 M4-12/13 的用例）'
assert_eq "0" "$GROUP_SKIPS" '整组 SKIP=0'
assert_eq "$GROUP_TESTS" "$GROUP_PASSED" "整组 passed = $GROUP_TESTS"

# ---------- 5) 变异反证 ----------
echo '==> 5. 变异反证：判据改错必须转红，恢复必须转绿'
mutate_case() { # $1=变异 id $2=期望转红的用例名（子串）
    local key="$1" expect="$2"
    python3 "$MUT" "$key" apply "$MUT_BAK" >/dev/null 2>&1 || { bad "变异 $key 施加失败"; return; }
    run_suite "$OUTDIR/04_mutation_${key}.out"
    local rc=$?
    local red
    red=$(grep -cE "^    business::erp::purchase::supplier_evaluation_suite_tests::${expect}$" "$OUTDIR/04_mutation_${key}.out" || true)
    if [ "$rc" -ne 0 ] && [ "$red" -ge 1 ]; then
        ok "变异 $key 被用例抓住（rc=$rc，$expect 转红）"
    else
        bad "变异 $key 没被抓住（rc=$rc，$expect 红行=$red）—— 用例没牙"
    fi
    python3 "$MUT" "$key" restore "$MUT_BAK" >/dev/null 2>&1
    run_suite "$OUTDIR/05_restored_${key}.out"
    local rc2=$?
    local green
    green=$(result_field "$OUTDIR/05_restored_${key}.out" passed)
    if [ "$rc2" -eq 0 ] && [ "$green" = "$SUITE_TESTS" ]; then
        ok "变异 $key 恢复后转绿（$green 条全过）"
    else
        bad "变异 $key 恢复后没转绿（rc=$rc2 passed=$green）"
    fi
}

mutate_case A grade_policy_holds_on_every_hundredth_point
mutate_case B rounding_happens_before_grading
mutate_case C grade_policy_holds_on_every_hundredth_point
mutate_case D sql_case_agrees_with_rust_on_every_hundredth_point

assert_eq "$GRADING_SHA_BEFORE" "$(sha256sum "$ROOT/$GRADING_SRC" | awk '{print $1}')" '打分模块已逐字回到变异前的基线（含本卡的读回对拍修复）'
assert_eq "$MODEL_SHA_BEFORE" "$(sha256sum "$ROOT/backend/app/src/business/erp/purchase/supplier_evaluation_model.rs" | awk '{print $1}')" '模型模块已逐字回到变异前的基线'
if [ -z "$(grep -rn 'MUTATION-' "$ROOT/$GRADING_SRC" "$ROOT/backend/app/src/business/erp/purchase/supplier_evaluation_model.rs" 2>/dev/null)" ]; then
    ok '工作区里没有残留的 MUTATION 标记'
else
    bad '工作区残留 MUTATION 标记（变异没恢复干净）'
fi

# ---------- 6) 不夹带 ----------
echo '== 6) 不夹带：本卡 0 迁移 / 0 前端改动 / 生产代码只动一处'
assert_eq "0" "$(git -C "$ROOT" status --porcelain -- backend/app/migrations | wc -l | tr -d ' ')" '本卡不动任何迁移文件'
assert_eq "1" "$([ -f "$ROOT/$SUITE_SRC" ] && echo 1 || echo 0)" '新增的测试文件在位'
assert_eq "0" "$(git -C "$ROOT" status --porcelain -- frontend/src frontend/tests | grep -cE 'm415|M415' || true)" '本卡 0 前端改动'
info "生产代码改动（应为 HEAD + 读回对拍修复）："
git -C "$ROOT" diff --stat -- "$GRADING_SRC" | sed 's/^/    /'

# ---------- 7) 门禁 ----------
echo '==> 7. 门禁：fmt / clippy --lib / clippy --all-targets / P2-62 夹具'
if [ "$SKIP_GATES" = "1" ]; then
    info 'SKIP_GATES=1 —— 跳过门禁'
else
    (cd "$BACKEND" && cargo fmt -p cloudpivot -- --check) >"$OUTDIR/06_fmt.out" 2>&1
    assert_eq "0" "$?" 'cargo fmt --check rc=0'
    (cd "$BACKEND" && cargo clippy -p cloudpivot --lib -- -D warnings) >"$OUTDIR/07_clippy_lib.out" 2>&1
    assert_eq "0" "$?" 'cargo clippy --lib -D warnings rc=0'
    (cd "$BACKEND" && cargo clippy -p cloudpivot --all-targets -- -D warnings) >"$OUTDIR/08_clippy_all_targets.out" 2>&1
    assert_eq "0" "$?" 'cargo clippy --all-targets -D warnings rc=0'
    assert_eq "0" "$(grep -c '^warning' "$OUTDIR/08_clippy_all_targets.out" || true)" 'clippy 输出 0 条 warning'
    (bash "$ROOT/scripts/verify/test_seed_gate.sh") >"$OUTDIR/09_test_seed_gate.out" 2>&1
    assert_eq "0" "$?" 'P2-62 夹具门禁 rc=0（本文件零主数据手写 INSERT）'
fi

echo
echo "================ M4-15 供应商评估单测演练汇总 ================"
echo "不连库：passed=$NO_DB_PASSED failed=$NO_DB_FAILED 本卡标记=$NO_DB_MARKS（假绿对照：真库用例提前 return）"
echo "真  库：passed=$DB_PASSED failed=$DB_FAILED SKIP=$DB_SKIPS 本卡标记=$DB_MARKS"
echo "回归组：passed=$GROUP_PASSED failed=$GROUP_FAILED SKIP=$GROUP_SKIPS"
echo "变异反证：A(含下界改不含) / B(舍入截断) / C(阈值 80→85) / D(SQL CASE 塞第二份阈值)"
printf '断言：PASS=%s FAIL=%s\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
    echo 'RESULT: FAIL'
    exit 1
fi
echo 'RESULT: PASS'
exit 0
