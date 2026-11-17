#!/usr/bin/env bash
# ============================================================
# M4-10 框架协议「单测」演练（含超限反例 + 变异反证）
#
# 卡：t_4da4c498（M4-10【M4·采购】框架协议：单测 · 含超限反例）
#
# 跑什么（每一步都对应卡面或新用例里的一句原话）：
#   ① 建演练库：从「已迁到 V0452」的源库克隆（模板库 cloudpivot_test 已落后到 V0357，
#      没有 biz_framework_agreement，直接拿它跑会 42P01 —— 见 README 的 finding）
#   ② 不连库：整组 framework_agreement 用例 passed=75 / failed=0，但 `[M4-10]` 正向标记 = 0
#      —— 这就是「假绿」：真库用例只是提前 return，passed 数看着一样
#   ③ 真库：同一批用例 SKIP=0、正向标记 = 12（本卡新增 12 条全跑了）
#   ④ 变异反证（4 个）：把判据临时改错 → 用例必须转红（且红的是**对应用例**）→ 恢复 → 转绿
#   ⑤ 门禁：cargo fmt --check / clippy --lib / clippy --all-targets
#
# 用法（99 开发机）：
#   bash scripts/verify/m410_framework_agreement_unit_drill.sh
#   DBHOST=127.0.0.1 bash …        # 145 测试机上跑（走 loopback）
#   环境变量：DB / SRC / DBUSER / DBPASS / KEEP_DB 可覆盖
#
# 退出码：0 = 全过；1 = 有断言失败；2 = 环境问题（建库 / 编译 / 源库落后）
# ============================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DBHOST="${DBHOST:-172.18.10.145}"
DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-cloudpivot}"
DBPASS="${DBPASS:-fbb1263691ff75c31ec99eaf258138b4}"
DBOWNER="${DBOWNER:-cloudpivot}"
DB="${DB:-cp_m410_ut_drill}"
# 源库：必须是已跑到 V0452 的库（默认用 145 的 dev 库；模板库 cloudpivot_test 落后，不能用）
SRC="${SRC:-cloudpivot_dev}"
KEEP_DB="${KEEP_DB:-0}"
OUTDIR="${OUTDIR:-$ROOT/docs/evidence/2026-11-18-m410-framework-agreement-unit}"

# 整组 framework_agreement 用例数（12 表结构 + 14 引用 + 12 执行量 + 12 本卡超限反例 = 50 真库；
# 另有 25 条纯逻辑），以及本卡新增的 12 条。
TOTAL_TESTS="${TOTAL_TESTS:-75}"
NEW_TESTS="${NEW_TESTS:-12}"
DB_TESTS="${DB_TESTS:-50}"

MUT="$ROOT/scripts/verify/m410_mutations.py"
REF="$ROOT/backend/app/src/business/erp/purchase/framework_agreement_ref.rs"
MUT_BAK="${TMPDIR:-/tmp}/m410-mutations"

PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() {
    if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi
}
assert_ge() {
    if [ "${1:-0}" -ge "${2:-0}" ] 2>/dev/null; then ok "$3（$1 ≥ $2）"; else bad "$3：期望 ≥ [$2] 实际 [$1]"; fi
}

psql_q() { PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -Atc "$1" 2>&1 | head -1; }
psql_src() { PGPASSWORD="$DBPASS" psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$SRC" -Atc "$1" 2>&1 | head -1; }

restore_all() {
    for key in A B C D; do
        python3 "$MUT" "$key" restore "$MUT_BAK" >/dev/null 2>&1
    done
}

cleanup_db() {
    restore_all
    if [ "$KEEP_DB" = "1" ]; then
        printf '    演练库保留：%s\n' "$DB"
        return
    fi
    PGPASSWORD="$DBPASS" dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" >/dev/null 2>&1
}
trap 'cleanup_db' EXIT

# 「test result: ok. 75 passed; 0 failed; 0 ignored」→ 三个数
result_field() { # $1=输出文件 $2=passed|failed|ignored（多测试目标时取最大值）
    grep -oE "test result: (ok|FAILED)\. [0-9]+ passed; [0-9]+ failed; [0-9]+ ignored" "$1" \
        | awk -v k="$2" '{for (i = 1; i <= NF; i++) if ($(i+1) == k";") {print $i; break}}' \
        | sort -n | tail -1
}

run_module() { # $1=输出文件；其余透传给 cargo test
    local out="$1"; shift
    (cd "$ROOT/backend" && cargo test --lib business::erp::purchase::framework_agreement "$@" -- --test-threads=1 --nocapture) >"$out" 2>&1
}

run_new_tests() { # $1=输出文件
    local out="$1"
    (cd "$ROOT/backend" && ERP_DB_URL="postgres://$DBUSER:$DBPASS@$DBHOST:$DBPORT/$DB" \
        cargo test --lib business::erp::purchase::framework_agreement_over_limit -- --test-threads=1) >"$out" 2>&1
}

mkdir -p "$OUTDIR"
echo "== 证据目录：$OUTDIR"

# ---------- ① 演练库 ----------
echo "==> 1. 建演练库：从 $SRC 克隆（模板库已落后，不能用）"
export PGPASSWORD="$DBPASS"
if [ "$(psql_src "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_name IN ('biz_framework_agreement','biz_framework_agreement_item')")" != "2" ]; then
    echo "FAIL: 源库 $SRC 没有框架协议两表（库落后于 V0450）—— 先把 $SRC 迁到最新再跑" >&2
    exit 2
fi
if [ "$(psql_src "SELECT count(*) FROM information_schema.columns WHERE table_name='biz_purchase_order' AND column_name='framework_agreement_id'")" != "1" ]; then
    echo "FAIL: 源库 $SRC 没有 biz_purchase_order.framework_agreement_id（库落后于 V0451）" >&2
    exit 2
fi
dropdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --if-exists "$DB" >/dev/null 2>&1
createdb -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -O "$DBOWNER" "$DB" || {
    echo 'FAIL: 建库失败（检查 DBHOST/DBPASS）' >&2; exit 2; }
pg_dump -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" --no-owner --no-privileges "$SRC" 2>/dev/null \
    | psql -q -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" >"$OUTDIR/00_restore.log" 2>&1 || {
    echo "FAIL: 从 $SRC 克隆失败（见 $OUTDIR/00_restore.log）" >&2; exit 2; }
ok "演练库 $DB 已从 $SRC 克隆"
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_name IN ('biz_framework_agreement','biz_framework_agreement_item')")" "2" '协议两表在位'
assert_eq "$(psql_q "SELECT count(*) FROM information_schema.views WHERE table_schema='public' AND table_name IN ('v_framework_agreement_execution','v_framework_agreement_item_execution')")" "2" 'M4-09 两个执行量视图在位（说明源库已到 V0452）'

# ---------- ② 不连库：假绿对照 ----------
echo '==> 2. 不连库：整组用例 passed 数一样，但正向标记 = 0（假绿）'
unset ERP_DB_URL
run_module "$OUTDIR/01_module_without_db.out"
RC=$?
NO_DB_PASSED=$(result_field "$OUTDIR/01_module_without_db.out" passed)
NO_DB_FAILED=$(result_field "$OUTDIR/01_module_without_db.out" failed)
NO_DB_MARKS=$(grep -c '\[M4-10\]' "$OUTDIR/01_module_without_db.out" || true)
NO_DB_COLLECTED=$(grep -cE "^test business::erp::purchase::framework_agreement" "$OUTDIR/01_module_without_db.out" || true)
assert_eq "0" "$RC" '不连库时返回 0（跳过的用例不算失败）'
assert_eq "0" "$NO_DB_FAILED" '不连库时 0 failed'
assert_eq "0" "$NO_DB_MARKS" '不连库时本卡正向标记 = 0（12 条真库用例一条都没真跑）'
assert_eq "$TOTAL_TESTS" "$NO_DB_COLLECTED" "整组 $TOTAL_TESTS 条用例都被收集（没被过滤器漏掉）"
assert_eq "$TOTAL_TESTS" "$NO_DB_PASSED" '不连库时 passed 数也是 75 —— passed 数**不能**用来判真假绿'

# ---------- ③ 真库：SKIP=0 ----------
echo '==> 3. 真库：同一批用例 SKIP=0，本卡 12 条正向标记全部出现'
export ERP_DB_URL="postgres://$DBUSER:$DBPASS@$DBHOST:$DBPORT/$DB"
run_module "$OUTDIR/02_module_with_db.out"
RC=$?
DB_PASSED=$(result_field "$OUTDIR/02_module_with_db.out" passed)
DB_FAILED=$(result_field "$OUTDIR/02_module_with_db.out" failed)
DB_SKIPS=$(grep -c 'SKIP: ' "$OUTDIR/02_module_with_db.out" || true)
DB_MARKS=$(grep -c '\[M4-10\]' "$OUTDIR/02_module_with_db.out" || true)
DB_RAN=$(grep -cE "^test business::erp::purchase::framework_agreement" "$OUTDIR/02_module_with_db.out" || true)
NEW_RAN=$(grep -cE "^test business::erp::purchase::framework_agreement_over_limit_tests::" "$OUTDIR/02_module_with_db.out" || true)
NEW_RED=$(grep -cE "^test business::erp::purchase::framework_agreement_over_limit_tests::.*FAILED" "$OUTDIR/02_module_with_db.out" || true)
assert_eq "0" "$RC" '真库整组返回 0'
assert_eq "0" "$DB_FAILED" '真库整组 0 failed'
assert_eq "0" "$DB_SKIPS" '真库整组 SKIP=0（一条都没跳过）'
assert_eq "$TOTAL_TESTS" "$DB_PASSED" "真库整组 passed = $TOTAL_TESTS"
assert_eq "$TOTAL_TESTS" "$DB_RAN" "整组 $TOTAL_TESTS 条逐条执行"
assert_eq "$NEW_TESTS" "$DB_MARKS" "本卡 $NEW_TESTS 条正向标记全部出现"
assert_eq "$NEW_TESTS" "$NEW_RAN" "本卡 $NEW_TESTS 条用例逐条执行（含超限反例）"
assert_eq "0" "$NEW_RED" "本卡 0 条 FAILED"
assert_ge "$DB_TESTS" "$NEW_TESTS" "整组里真库用例共 $DB_TESTS 条"

# ---------- ④ 变异反证 ----------
echo '==> 4. 变异反证：把判据改错必须转红，恢复必须转绿'
mutate_case() { # $1=变异 id $2=期望转红的用例名（子串）
    local key="$1" expect="$2"
    python3 "$MUT" "$key" apply "$MUT_BAK" >/dev/null 2>&1 || {
        bad "变异 $key 施加失败"; return; }
    run_new_tests "$OUTDIR/04_mutation_${key}.out"
    local rc=$?
    local red_line
    red_line=$(grep -cE "^test .*${expect}.* \.\.\. FAILED" "$OUTDIR/04_mutation_${key}.out" || true)
    if [ "$rc" -ne 0 ] && [ "$red_line" -ge 1 ]; then
        ok "变异 $key 被用例抓住（rc=$rc，$expect 转红）"
    else
        bad "变异 $key 没被抓住（rc=$rc，$expect 红行=$red_line）—— 用例没牙"
    fi
    python3 "$MUT" "$key" restore "$MUT_BAK" >/dev/null 2>&1
    run_new_tests "$OUTDIR/05_restored_${key}.out"
    local rc2=$?
    local green
    green=$(result_field "$OUTDIR/05_restored_${key}.out" passed)
    if [ "$rc2" -eq 0 ] && [ "$green" = "$NEW_TESTS" ]; then
        ok "变异 $key 恢复后转绿（$green 条全过）"
    else
        bad "变异 $key 恢复后没转绿（rc=$rc2 passed=$green）"
    fi
}

mutate_case A split_lines_on_same_material_cannot_bypass_cap
mutate_case B head_total_qty_cap_allows_exact_boundary
mutate_case C tax_inclusive_total_amount_cap_counts_with_tax
mutate_case D head_total_amount_cap_allows_exact_boundary

# 反证完必须一字不差地回到 HEAD 版本（变异绝不能留在工作区里）
if git -C "$ROOT" diff --quiet -- "$REF"; then
    ok '生产代码已恢复（与 HEAD 逐字一致）'
else
    bad "生产代码没恢复干净：git diff $REF 有残留"
fi

# ---------- ⑤ 门禁 ----------
echo '==> 5. 门禁：fmt --check / clippy --lib / clippy --all-targets'
(cd "$ROOT/backend" && cargo fmt -p cloudpivot -- --check) >"$OUTDIR/06_fmt.out" 2>&1
assert_eq "0" "$?" 'cargo fmt --check rc=0'
(cd "$ROOT/backend" && cargo clippy --lib -- -D warnings) >"$OUTDIR/07_clippy_lib.out" 2>&1
assert_eq "0" "$?" 'cargo clippy --lib -D warnings rc=0'
(cd "$ROOT/backend" && cargo clippy --all-targets -- -D warnings) >"$OUTDIR/08_clippy_all_targets.out" 2>&1
assert_eq "0" "$?" 'cargo clippy --all-targets -D warnings rc=0'
assert_eq "$(grep -c '^warning' "$OUTDIR/08_clippy_all_targets.out" || true)" "0" 'clippy 输出 0 条 warning'

echo
echo "================ M4-10 框架协议单测演练汇总 ================"
echo "不连库：passed=$NO_DB_PASSED failed=$NO_DB_FAILED 本卡标记=$NO_DB_MARKS（假绿对照）"
echo "真  库：passed=$DB_PASSED failed=$DB_FAILED SKIP=$DB_SKIPS 本卡标记=$DB_MARKS"
echo "变异反证：A/B/C/D 各「转红 → 恢复转绿」一轮"
printf '断言：PASS=%s FAIL=%s\n' "$PASS" "$FAIL"
if [ "$FAIL" -ne 0 ]; then
    echo 'RESULT: FAIL'
    exit 1
fi
echo 'RESULT: PASS'
exit 0
