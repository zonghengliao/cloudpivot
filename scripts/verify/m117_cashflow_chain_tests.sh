#!/usr/bin/env bash
# ============================================================================
# M1-17 现金流底稿单测（含勾稽闭合）+ 三次「反证」——可复现脚本
#
# 卡：t_7e9df7bf（M1-17【M1·财务】现金流底稿：单测（含勾稽闭合））
#
# 做的事（全部对**临时库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆一个 isolate 库（库名带 test，脚本自己 DROP）
#   2. 补前置迁移：V0414（底稿表）/ V0416（间接法调整项表，M1-15 的用例也要它）
#      —— 模板库还没跑到这两版，缺了就是「表不存在」而不是「用例失败」
#   3. 跑本卡的单测（`business::erp::cashflow_chain_tests`，11 条，真库）
#   4. 跑现金流全家族（`business::erp::cashflow*`，53 条：M1-14 的 12 +
#      M1-15 的 12 + M1-16 的 18 + 本卡 11），证明本卡没有踩坏别人的用例
#   5. 无 `ERP_DB_URL` 时同一批用例必须全部 SKIP（不静默假绿）
#   6. **反证**（三次，每次单独施加 → 断言指定用例转红 → git 还原）：
#        A 把 `reconcile` 的差额 round 到 2 位（尾差被抹平）
#          ⇒ sub_cent_tail_is_not_rounded_away 必须红
#        B 让 `ensure_exportable` 无条件放行（出表闸失效）
#          ⇒ missing_closing_row / sub_cent_tail / wrong_closing 三条必须红
#        C 让 `previous_period_of` 只减月份不减年（跨年给出 1913-00）
#          ⇒ previous_period_is_previous_accounting_period_across_years 与
#            previous_period_not_opened_leaves_previous_column_null 必须红
#      反证是这套用例的「效力证明」：全绿只能说明跑通了，能红才说明它在守着东西。
#
# 用法：
#   bash scripts/verify/m117_cashflow_chain_tests.sh                # 99 → 145（默认）
#   PG_HOST=127.0.0.1 bash scripts/verify/m117_cashflow_chain_tests.sh   # 在 145 上跑
#   ONLY=mutation bash scripts/verify/m117_cashflow_chain_tests.sh # 只跑反证
#
# 环境变量：PG_HOST(172.18.10.145) / PG_PORT(5432) / PG_USER(cloudpivot) /
#           PGPASSWORD（145 开发库口令，仓库内既有脚本同值）/ TEMPLATE_DB(cloudpivot_test)
#           ONLY=all|mutation|test   （默认 all）
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BACKEND="$ROOT/backend"
MIG="$ROOT/backend/app/migrations/business"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m117_chain_$$"
ONLY="${ONLY:-all}"

TMP="${TMPDIR:-/tmp}"
mkdir -p "$TMP"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }
head2() { printf '\n== %s ==\n' "$1"; }

psql_q() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
runfile() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$1" >"$TMP/m117_mig.log" 2>&1; }

cleanup() {
    dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1
    # 只还原**本脚本自己施加过反证**的文件；绝不 checkout 别人的未提交改动
    local rel
    for rel in "${MUTATED_FILES[@]:-}"; do
        [ -n "$rel" ] && git -C "$ROOT" checkout -- "$rel" >/dev/null 2>&1
    done
}
trap cleanup EXIT

MUTATED_FILES=()

echo "== M1-17 现金流底稿单测 + 反证 =="
echo "   PG=${PG_HOST}:${PG_PORT} 模板=${TEMPLATE_DB} 临时库=${DB} ONLY=${ONLY}"

# ---------- 0) 建库 + 前置迁移 ----------
if ! createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$TMP/m117_createdb.err"; then
    echo "  ⛔ 建库失败：$(cat "$TMP/m117_createdb.err")"
    exit 1
fi
info "临时库已从 $TEMPLATE_DB 克隆"

if [ "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_worksheet';")" = "0" ]; then
    runfile "$MIG/V0414__cashflow_worksheet.up.sql" && ok "V0414 前置（底稿表）" || { bad "V0414 前置失败：$(tail -3 "$TMP/m117_mig.log")"; exit 1; }
fi
if [ "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_adjust_item';")" = "0" ]; then
    runfile "$MIG/V0416__cashflow_adjust_item.up.sql" && ok "V0416 前置（间接法调整项表）" || { bad "V0416 前置失败：$(tail -3 "$TMP/m117_mig.log")"; exit 1; }
fi
for table in biz_cashflow_worksheet biz_cashflow_adjust_item biz_cashflow_item biz_accounting_period; do
    if [ "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='$table';")" = "1" ]; then
        ok "前置表存在：$table"
    else
        bad "前置表缺失：$table"
    fi
done

run_cargo() { # run_cargo <log 文件> <filter> [环境前缀]
    local log="$1" filter="$2"
    ( cd "$BACKEND" && ERP_DB_URL="postgres://${PG_USER}:${PGPASSWORD}@${PG_HOST}:${PG_PORT}/${DB}" \
        cargo test --lib "$filter" -- --test-threads=1 ) >"$log" 2>&1
}

if [ "$ONLY" = "all" ] || [ "$ONLY" = "test" ]; then
    # ---------- 1) 本卡用例 ----------
    head2 "1) 本卡单测：business::erp::cashflow_chain_tests（11 条，真库）"
    run_cargo "$TMP/m117_chain.log" business::erp::cashflow_chain_tests
    chain_result="$(grep -E '^test result:' "$TMP/m117_chain.log" | head -1)"
    echo "  $chain_result"
    if echo "$chain_result" | grep -q "ok. 11 passed; 0 failed"; then
        ok "本卡 11/11 全绿"
    else
        bad "本卡用例未全绿：$(grep -E '^test .* FAILED|panicked at' "$TMP/m117_chain.log" | head -5)"
    fi
    skipped="$(grep -c 'SKIP:' "$TMP/m117_chain.log" || true)"
    if [ "$skipped" = "0" ]; then ok "真库用例没有一条 SKIP（没有静默假绿）"; else bad "有 $skipped 条 SKIP：$(grep 'SKIP:' "$TMP/m117_chain.log" | head -2)"; fi

    # ---------- 2) 现金流全家族回归 ----------
    head2 "2) 现金流全家族：business::erp::cashflow*（M1-14/15/16 + 本卡）"
    run_cargo "$TMP/m117_family.log" business::erp::cashflow
    family_result="$(grep -E '^test result:' "$TMP/m117_family.log" | head -1)"
    echo "  $family_result"
    if echo "$family_result" | grep -q "0 failed"; then
        ok "全家族无红（$family_result）"
    else
        bad "全家族有红：$(grep -E '^test .* FAILED' "$TMP/m117_family.log" | head -5)"
    fi

    # ---------- 3) 无库时 SKIP ----------
    head2 "3) 无 ERP_DB_URL：真库用例必须 SKIP，纯逻辑用例照跑"
    # 注意必须带 --nocapture：libtest 会把**通过**的用例的 stdout 吞掉，SKIP 提示就看不见了，
    # 于是「没连库」这件事只能靠 --nocapture 证出来（不带它就是在自欺）。
    ( cd "$BACKEND" && env -u ERP_DB_URL cargo test --lib business::erp::cashflow_chain_tests -- --nocapture --test-threads=1 ) \
        >"$TMP/m117_skip.log" 2>&1
    skip_result="$(grep -E '^test result:' "$TMP/m117_skip.log" | head -1)"
    echo "  $skip_result"
    if grep -q 'SKIP: 未设置 ERP_DB_URL' "$TMP/m117_skip.log"; then
        ok "无库时打印 SKIP（不连库、不假绿）"
    else
        bad "无库时没打印 SKIP（可能在偷偷连库或静默通过）"
    fi
    # `ensure_db()` 用 `OnceCell` 缓存，所以整个进程只打**一次** SKIP（不是每条用例一次）。
    if [ "$(grep -c 'SKIP: 未设置 ERP_DB_URL' "$TMP/m117_skip.log")" = "1" ]; then
        ok "SKIP 只打一次（OnceCell 缓存，符合实现）"
    else
        bad "SKIP 条数不符：$(grep -c 'SKIP: 未设置 ERP_DB_URL' "$TMP/m117_skip.log")（期望 1）"
    fi
    # 11 条仍然「通过」——但真库那 9 条是**空跑**（早退），这一点由上面的 SKIP + 0.00s 证明
    if echo "$skip_result" | grep -q "ok. 11 passed"; then
        ok "纯逻辑 2 条照跑 + 真库 9 条空跑（全部 SKIP，不是假绿）"
    else
        bad "无库时用例结果异常：$skip_result"
    fi
fi

# ---------- 4) 反证（三次） ----------
mutate() { # mutate <workdir 文件相对路径> <python 代码>
    local rel="$1" code="$2"
    python3 - "$ROOT/$rel" "$code" <<'PY'
import sys
path, mode = sys.argv[1], sys.argv[2]
src = open(path).read()
if mode == "A":
    old = "        (Some(expected), Some(actual)) => Some(expected - actual),"
    new = "        (Some(expected), Some(actual)) => Some((expected - actual).round_dp(2)),"
elif mode == "B":
    old = "    if statement.check.balanced {"
    new = "    if statement.check.balanced || true {"
elif mode == "C":
    old = "    let (year, month) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };"
    new = "    let (year, month) = (year, month - 1);"
else:
    raise SystemExit("unknown mode " + mode)
if old not in src:
    raise SystemExit("锚点未找到（代码已变，请先更新反证锚点）：" + mode)
open(path, "w").write(src.replace(old, new, 1))
PY
    return $?
}

reverse_proof() { # reverse_proof <A|B|C> <文件相对路径> <断言输出里必须出现的用例名>...
    local mode="$1" rel="$2"; shift 2
    local expect=("$@")
    head2 "反证 $mode：$rel"
    if ! git -C "$ROOT" diff --quiet -- "$rel"; then
        bad "反证前 $rel 不干净（有别人的未提交改动），跳过反证 —— 不能拿别人的改动做反证"
        return
    fi
    if ! mutate "$rel" "$mode" 2>"$TMP/m117_mut.err"; then
        bad "施加反证失败：$(cat "$TMP/m117_mut.err")"
        git -C "$ROOT" checkout -- "$rel"
        return
    fi
    MUTATED_FILES+=("$rel")
    info "已施加反证：$mode"
    run_cargo "$TMP/m117_mut_$mode.log" business::erp::cashflow_chain_tests
    local result
    result="$(grep -E '^test result:' "$TMP/m117_mut_$mode.log" | head -1)"
    echo "  $result"
    if echo "$result" | grep -q "FAILED"; then
        ok "反证生效：用例转红（$result）"
    else
        bad "反证没生效：代码被改了用例还是全绿 —— 这条链没有被守住"
    fi
    local name
    for name in "${expect[@]}"; do
        if grep -q "^test business::erp::cashflow_chain_tests::$name .* FAILED" "$TMP/m117_mut_$mode.log"; then
            ok "预期转红的用例确实红了：$name"
        else
            bad "预期该红的用例没红：$name"
        fi
    done
    git -C "$ROOT" checkout -- "$rel"
    MUTATED_FILES=("${MUTATED_FILES[@]/$rel/}")
    if git -C "$ROOT" diff --quiet -- "$rel"; then
        ok "反证后已还原：$rel"
    else
        bad "反证后没能还原：$rel（请人工 git checkout）"
    fi
}

if [ "$ONLY" = "all" ] || [ "$ONLY" = "mutation" ]; then
    reverse_proof A backend/app/src/business/erp/cashflow_worksheet.rs \
        sub_cent_tail_is_not_rounded_away
    reverse_proof B backend/app/src/business/erp/cashflow_statement.rs \
        missing_closing_row_is_named_and_blocks_export \
        sub_cent_tail_is_not_rounded_away \
        wrong_closing_keeps_page_open_but_blocks_export
    reverse_proof C backend/app/src/business/erp/report/mod.rs \
        previous_period_is_previous_accounting_period_across_years \
        previous_period_not_opened_leaves_previous_column_null
fi

head2 "结果"
echo "  PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -eq 0 ]; then
    echo "  ✅ 全部通过"
    exit 0
fi
echo "  ❌ 有失败"
exit 1
