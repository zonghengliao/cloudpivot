#!/usr/bin/env bash
# ============================================================================
# m211_aging_period_mutation_drill.sh —— 库龄「跨期」单测的**反证演练**（M2-11）
#
# 卡：t_855bc226（M2-11【M2·仓库】库龄：单测 · 含跨期与呆滞判定）
#
# 为什么要有这个脚本：
#   单测全绿只说明「现在这样算是绿的」，不说明「改错了会红」。跨期口径的错法
#   （拿过账时间当业务日期、期末/期初当天用 `<` 而不是 `<=`）**不会让任何接口报错**，
#   只会让某个月末的数字少一层货。所以本脚本把 V0424 库函数的判据**故意改错两次**，
#   每次都要求 `aging_period_tests` 立刻变红；改回正确实现后必须重新全绿。
#
#   两次变异（都是真实项目里见过的错法）：
#     A. `biz_date <= p_as_of` → `(created_at AT TIME ZONE 'Asia/Shanghai')::date <= p_as_of`
#        —— 用**过账时间**当期末快照判据 ⇒ 补录单据（业务日期本期、过账下期）会漏；
#     B. `biz_date <= p_as_of` → `biz_date < p_as_of`
#        —— 期末当天用**开区间** ⇒ 业务日期 == 基准日的单据（当天进 / 当天出）会漏。
#
# 用法（在 99 或 145 都能跑；默认连 145 的 PG，只有库名可改）：
#   bash scripts/verify/m211_aging_period_mutation_drill.sh [库名]
#
# 约束：
#   * 库名必须含 drill / e2e / test（防手滑指向生产库），脚本自己 DROP 同名前缀的旧库；
#   * 只在**克隆库**上动函数（生产库别跑变异）；结束后恢复成迁移原文并 DROP 库。
# ============================================================================
set -uo pipefail

DB="${1:-cp_m211_mutation_drill}"
HOST="${PGHOST:-172.18.10.145}"
PGUSER_NAME="${PGUSER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BACKEND="$REPO_ROOT/backend"
MIGRATIONS="$BACKEND/app/migrations/business"
TEST_FILTER="business::erp::inventory_aging::aging_period_tests"

PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf '  PASS  %s\n' "$1"; }
fail() { FAIL=$((FAIL + 1)); printf '  FAIL  %s\n' "$1"; }

psql_db() { psql -h "$HOST" -U "$PGUSER_NAME" -d "$DB" -v ON_ERROR_STOP=1 -q "$@"; }

case "$DB" in
    *drill* | *e2e* | *test*) ;;
    *)
        echo "拒绝执行：库名 '$DB' 不含 drill/e2e/test（防手滑指向业务库）" >&2
        exit 2
        ;;
esac

cleanup() {
    # 恢复正确实现（库函数在迁移里，重放 up 即可；函数是 CREATE OR REPLACE）
    if psql -h "$HOST" -U "$PGUSER_NAME" -d "$DB" -q -v ON_ERROR_STOP=1 \
        -f "$MIGRATIONS/V0424__inventory_aging_calc.up.sql" >/dev/null 2>&1; then
        :
    fi
    dropdb -h "$HOST" -U "$PGUSER_NAME" --if-exists "$DB" >/dev/null 2>&1
}
trap cleanup EXIT

echo "== M2-11 跨期单测反证演练 =="
echo "库：$DB @ $HOST   （模板 cloudpivot_test，脚本自己建 / 删）"

dropdb -h "$HOST" -U "$PGUSER_NAME" --if-exists "$DB" >/dev/null 2>&1
if ! createdb -h "$HOST" -U "$PGUSER_NAME" -T cloudpivot_test "$DB"; then
    echo "建库失败（模板 cloudpivot_test 不在？）" >&2
    exit 2
fi
pass "克隆模板库"

for migration in V0424__inventory_aging_calc V0425__inventory_aging_report V0426__inventory_slow_moving; do
    if psql_db -f "$MIGRATIONS/$migration.up.sql" >/dev/null 2>&1; then
        pass "迁移 $migration"
    else
        fail "迁移 $migration（psql 报错）"
        exit 1
    fi
done

# 变异 A / B 的通用实现：把迁移里的库函数定义切出来、替换一句判据，再 CREATE OR REPLACE 回去。
#
# 为什么不在 psql 里用 `DO $$ … replace(src, '…') $$` 做：函数体的判据里带单引号
# （`AT TIME ZONE 'Asia/Shanghai'`），SQL 字符串字面量与 psql 变量插值三层引号互相打架，
# 写出来的转义比被测逻辑还难读。切文件 + 文本替换 + psql -f 更直白，也更容易核对。
mutate() {
    local from="$1" to="$2" label="$3"
    local fn_file
    fn_file="${TMPDIR:-/tmp}/m211_aging_fn.$$.sql"
    awk '/^CREATE OR REPLACE FUNCTION biz_inventory_aging_layers/,/^\$\$;/' \
        "$MIGRATIONS/V0424__inventory_aging_calc.up.sql" >"$fn_file"
    if ! grep -qF "$from" "$fn_file"; then
        fail "注入变异：$label（变异目标不在函数体里：$from）"
        rm -f "$fn_file"
        exit 1
    fi
    python3 - "$fn_file" "$from" "$to" <<'PY'
import pathlib
import sys

path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
text = pathlib.Path(path).read_text(encoding="utf-8")
if old not in text:
    raise SystemExit(f"变异目标不在函数体里：{old}")
pathlib.Path(path).write_text(text.replace(old, new, 1), encoding="utf-8")
PY
    if [ $? -eq 0 ] && psql_db -f "$fn_file" >/dev/null 2>&1; then
        pass "注入变异：$label"
    else
        fail "注入变异：$label"
        rm -f "$fn_file"
        exit 1
    fi
    rm -f "$fn_file"
}

restore_function() {
    if psql_db -f "$MIGRATIONS/V0424__inventory_aging_calc.up.sql" >/dev/null 2>&1; then
        pass "恢复 V0424 正确实现"
    else
        fail "恢复 V0424 正确实现"
        exit 1
    fi
}

# 跑一组用例，返回「通过条数 / 失败条数」到全局 RUN_OK / RUN_FAIL
run_tests() {
    local out line
    out=$(cd "$BACKEND" && ERP_DB_URL="postgres://$PGUSER_NAME:$PGPASSWORD@$HOST:5432/$DB" \
        cargo test --lib "$TEST_FILTER" -- --test-threads=1 2>&1)
    # lib 测试二进制的结果行（云枢还有 kernel / doc 两个 0 用例的二进制，取第一条）
    line=$(printf '%s\n' "$out" | grep -E '^test result: (ok|FAILED)\.' | head -1)
    RUN_OK=$(printf '%s\n' "$line" | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+')
    RUN_FAIL=$(printf '%s\n' "$line" | grep -oE '[0-9]+ failed' | grep -oE '[0-9]+')
    RUN_OUT="$out"
    RUN_OK="${RUN_OK:-0}"
    RUN_FAIL="${RUN_FAIL:-0}"
}

# ---------- ① 基线：正确实现必须全绿 ----------
run_tests
if [ "$RUN_FAIL" -eq 0 ] && [ "$RUN_OK" -eq 11 ]; then
    pass "基线（正确实现）11 条全绿"
else
    fail "基线应为 11 passed / 0 failed，实际 $RUN_OK / $RUN_FAIL"
    printf '%s\n' "$RUN_OUT" | tail -20
fi

# ---------- ② 变异 A：期末快照按过账时间切（补录单据会漏期）----------
mutate "WHERE biz_date <= p_as_of" "WHERE (created_at AT TIME ZONE 'Asia/Shanghai')::date <= p_as_of" \
    "A：期末快照改用过账时间 created_at"
run_tests
caught_a=$(printf '%s\n' "$RUN_OUT" | grep -cE "^    business::erp::inventory_aging::aging_period_tests::")
if [ "$RUN_FAIL" -ge 1 ]; then
    pass "变异 A 被抓住：$RUN_OK 通过 / $RUN_FAIL 条变红（其中本卡 $caught_a 条）"
else
    fail "变异 A 未被抓住（passed=$RUN_OK failed=$RUN_FAIL）—— 跨期单测形同虚设"
    printf '%s\n' "$RUN_OUT" | tail -20
fi
if printf '%s\n' "$RUN_OUT" | grep -q "aging_period_tests::backdated_document_lands_in_its_business_period"; then
    pass "变异 A 由「补录单据按业务日期归期」用例点名抓住"
else
    fail "变异 A 没有让「补录单据」用例变红（它是最该报错的那条）"
fi
restore_function

# ---------- ③ 变异 B：期末当天用开区间（当天的进 / 出会被漏）----------
mutate "WHERE biz_date <= p_as_of" "WHERE biz_date < p_as_of" "B：期末快照改成 biz_date < p_as_of（当天漏掉）"
run_tests
caught_b=$(printf '%s\n' "$RUN_OUT" | grep -cE "^    business::erp::inventory_aging::aging_period_tests::")
if [ "$RUN_FAIL" -ge 1 ]; then
    pass "变异 B 被抓住：$RUN_FAIL 条用例变红（其中本卡 $caught_b 条）"
else
    fail "变异 B 未被抓住（passed=$RUN_OK failed=$RUN_FAIL）"
    printf '%s\n' "$RUN_OUT" | tail -20
fi
for name in period_boundary_day_is_inclusive opening_layer_dates_from_period_start; do
    if printf '%s\n' "$RUN_OUT" | grep -q "aging_period_tests::$name"; then
        pass "变异 B 由「$name」用例抓住"
    else
        fail "变异 B 没有让「$name」用例变红"
    fi
done
restore_function

# ---------- ④ 收尾：恢复后必须重新全绿（证明变异只改了库函数，没污染夹具）----------
run_tests
if [ "$RUN_FAIL" -eq 0 ] && [ "$RUN_OK" -eq 11 ]; then
    pass "恢复后重新全绿（11 条）"
else
    fail "恢复后应为 11 passed / 0 failed，实际 $RUN_OK / $RUN_FAIL"
    printf '%s\n' "$RUN_OUT" | tail -20
fi

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
if [ "$FAIL" -ne 0 ]; then
    echo "RESULT: FAIL"
    exit 1
fi
echo "RESULT: ALL PASS"
