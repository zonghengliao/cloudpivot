#!/usr/bin/env bash
# ============================================================================
# test_budget_gate.sh —— 门禁：耗时预算告警「真抓得住慢单测、也不静默合格」（P2-64）
#
# 为什么需要它：
#   耗时预算（单测 > 5000 ms 就提示）是个**只会输出文本**的东西，坏掉的形态有两种，
#   两种都不会自己喊：
#     ① **漏报**：边界写成 `>=` / 阈值单位错（秒当毫秒）/ 拿 0.000 当耗时 ⇒ 慢用例被放过；
#     ② **假合格**：没量到逐条耗时（from-logs 回填的日志里 time 全是 0.000）时
#        报「全部在预算内」—— CI 一片绿，其实一个都没量（和 P2-63 的静默降级同一类病）。
#   所以这道闸不校验「有没有输出」，而是校验**判据的边界与反证**：
#     · 固定夹具 1.200 / 4.999 / **5.000** / 5.001 s：只抓 5.001（卡面口径是「> 5000」）；
#     · 阈值 5001 ms ⇒ 0 条超（不误报）；阈值 1 ms ⇒ 4 条全抓（不漏报）；
#     · 没量到耗时的夹具 ⇒ 必须说「预算未判定」，**不得**出现「全部在预算内」；
#     · `--strict` 有超预算 exit 1、`--require-measured` 没数据 exit 2；
#     · 接线不能被悄悄摘掉（test_domain.py 的 --budget-ms / 调用点 / check.sh 的挂载）。
#
# 纯 stdlib + 固定夹具，不编译、不连库（L0 秒级）。
#
# 用法：
#   bash scripts/verify/test_budget_gate.sh
#   bash scripts/verify/test_budget_gate.sh --selftest   # 同一件事（显式点名反证）
#
# 退出码：0 = 全过；1 = 有断言失败（明细在 stdout/stderr）；2 = 前置缺失
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
GEN="$ROOT/scripts/junit_report.py"
RUNNER="$ROOT/scripts/test_domain.py"
FIX="$ROOT/scripts/verify/fixtures"
SAMPLE="$FIX/budget_sample.xml"
UNMEASURED="$FIX/budget_unmeasured.xml"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/p264-budget-gate.XXXXXX")"

BAD=0
ok()   { echo "✓ $*"; }
bad()  { echo "✗ $*" >&2; BAD=$((BAD + 1)); }

[ -f "$GEN" ] || { echo "==> 前置缺失：$GEN" >&2; exit 2; }
[ -f "$RUNNER" ] || { echo "==> 前置缺失：$RUNNER" >&2; exit 2; }
[ -f "$SAMPLE" ] || { echo "==> 前置缺失：$SAMPLE" >&2; exit 2; }
[ -f "$UNMEASURED" ] || { echo "==> 前置缺失：$UNMEASURED" >&2; exit 2; }

# 跑一次 budget，把 stdout+stderr 落文件，返回码单独取（不要在管道里丢 rc）
run_budget() { # <输出文件> <参数…>
    local out=$1; shift
    python3 "$GEN" budget "$@" >"$out" 2>&1
    echo $?
}
expect_rc()    { [ "$2" = "$3" ] && ok "$1（rc=$3）" || bad "$1：期望 rc=$2，实际 rc=$3"; }
expect_has()   { grep -qF -- "$2" "$3" && ok "$1" || bad "$1：输出里没有『$2』（见 $3）"; }
expect_lacks() { grep -qF -- "$2" "$3" && bad "$1：不该出现『$2』（见 $3）" || ok "$1"; }

echo "==> test_budget_gate：预算判据边界 + 反证 + 接线（源：${GEN#$ROOT/}）"

# —— ① 解析器自检（含 ⑰–㉒ 的预算断言：命中/阈值/边界/XML 往返/未量不算合格/接线）——
echo
echo "-- ① 解析器自检（scripts/junit_report.py selftest）"
if python3 "$GEN" selftest >"$TMP/selftest.txt" 2>&1; then
    ok "selftest 通过（$(grep -c '^✓' "$TMP/selftest.txt") 条断言，含预算 ⑰–㉒）"
else
    bad "selftest 未通过（见 $TMP/selftest.txt）"
    tail -20 "$TMP/selftest.txt" >&2
fi

# —— ② 夹具本身得是合法 JUnit（否则下面的断言全在判一个坏文件）——
echo
echo "-- ② 夹具结构自洽（scripts/junit_report.py check）"
if python3 "$GEN" check "$SAMPLE" "$UNMEASURED" >"$TMP/check.txt" 2>&1; then
    ok "两份夹具通过 JUnit 结构校验"
else
    bad "夹具结构不自洽（见 $TMP/check.txt）"
    cat "$TMP/check.txt" >&2
fi

# —— ③ 默认阈值 5000 ms：只抓 5.001 s，边界 5.000 s 不抓 ——
echo
echo "-- ③ 默认阈值（5000 ms）边界：> 才算超"
rc=$(run_budget "$TMP/def.txt" "$SAMPLE" --top 0)
expect_rc "默认档不阻断（只提示）" 0 "$rc"
expect_has  "抓到 1 条超预算" "1 条超预算" "$TMP/def.txt"
expect_has  "超的是 5.001 s 那条" "t04_over_5001ms" "$TMP/def.txt"
expect_lacks "正好 5.000 s 不算超（口径是「>」不是「>=」）" "t03_edge_exactly_5000ms" "$TMP/def.txt"
expect_has  "报出实测耗时（5001 ms）" "5001.0 ms" "$TMP/def.txt"

# —— ④ 阈值上抬 1 ms ⇒ 一条都不超（不误报）—— 与 ③ 成对，证明判定真跟着阈值走 ——
echo
echo "-- ④ 阈值 5001 ms ⇒ 0 条超（不误报）"
rc=$(run_budget "$TMP/high.txt" "$SAMPLE" --limit-ms 5001 --top 0 --strict)
expect_rc "0 条超时 --strict 也 exit 0" 0 "$rc"
expect_has "口头结论是『全部在预算内』" "全部在预算内" "$TMP/high.txt"

# —— ⑤ 阈值压低到 1 ms ⇒ 4 条全抓（不漏报，含 1.200 s 那条）——
echo
echo "-- ⑤ 阈值 1 ms ⇒ 4 条全抓（不漏报）"
rc=$(run_budget "$TMP/low.txt" "$SAMPLE" --limit-ms 1 --top 0)
expect_rc "压低阈值不阻断" 0 "$rc"
expect_has "抓到 4 条" "4 条超预算" "$TMP/low.txt"
expect_has "最慢的那条也在" "t01_fast_1200ms" "$TMP/low.txt"

# —— ⑥ --strict：有超预算必须 exit 1（能被 CI 当门禁用）——
echo
echo "-- ⑥ --strict 有超预算 ⇒ exit 1"
rc=$(run_budget "$TMP/strict.txt" "$SAMPLE" --top 0 --strict)
expect_rc "严格档阻断" 1 "$rc"

# —— ⑦ 反证：没量到耗时（from-logs 的 0.000）**不能**算合格 ——
echo
echo "-- ⑦ 反证：未量的套件不得被判成『全部在预算内』"
rc=$(run_budget "$TMP/unm.txt" "$UNMEASURED" --top 0)
expect_rc "未量时默认不阻断（但要说清未判定）" 0 "$rc"
expect_has  "明确说『预算未判定』" "预算未判定" "$TMP/unm.txt"
expect_has  "报出未量条数" "2 条未量" "$TMP/unm.txt"
expect_lacks "**不得**出现『全部在预算内』" "全部在预算内" "$TMP/unm.txt"
rc=$(run_budget "$TMP/unm2.txt" "$UNMEASURED" --top 0 --require-measured)
expect_rc "--require-measured 时 exit 2（防静默降级）" 2 "$rc"

# —— ⑧ 反证：判据坏了必须被这道闸抓住（造 2 种畸形，逐个验证判据真的在读数据）——
echo
echo "-- ⑧ 反证：判据坏了会被抓住"
# (a) 判定必须真读 testcase/@time：把 1.200 s 那条改成 1 ms ⇒ 阈值 1 ms 下超预算从 4 条变 3 条
cp "$SAMPLE" "$TMP/mut_units.xml"
sed -i 's/name="t01_fast_1200ms" time="1.200"/name="t01_fast_1200ms" time="0.001"/' "$TMP/mut_units.xml"
rc=$(run_budget "$TMP/mut_units.txt" "$TMP/mut_units.xml" --limit-ms 1 --top 0)
if grep -qF "3 条超预算" "$TMP/mut_units.txt" && ! grep -qF "4 条超预算" "$TMP/mut_units.txt"; then
    ok "反证 (a) 判定确实读 testcase/@time（改小 1 条后 4 条超 → 3 条超）"
else
    bad "反证 (a) 未生效：改小一格耗时后超预算条数没变（判定没读 @time）"
fi
# (b) from-logs 形态里若把 per_testcase_time 改成 measured ⇒ 会静默报「全部合格」，
#     这正是 ⑦ 要拦的形态；这里证明「那形态造得出来」，⑦ 的断言才不是空转
sed 's/value="unavailable"/value="measured"/' "$UNMEASURED" >"$TMP/mut_measured.xml"
rc=$(run_budget "$TMP/mut_measured.txt" "$TMP/mut_measured.xml" --top 0)
if grep -qF "全部在预算内" "$TMP/mut_measured.txt"; then
    ok "反证 (b) 静默合格形态造得出来（正因如此 ⑦ 必须断言『预算未判定』）"
else
    bad "反证 (b) 造不出静默合格形态 —— 说明 ⑦ 的断言打偏了（没覆盖真正的失败形态）"
fi

# —— ⑨ 接线：CLI 开关与调用点不能被悄悄摘掉 ——
echo
echo "-- ⑨ 接线（test_domain.py ↔ junit_report.py ↔ check.sh）"
if python3 "$RUNNER" run --help 2>&1 | grep -q -- "--budget-ms"; then
    ok "test_domain.py run 仍有 --budget-ms"
else
    bad "test_domain.py run 的 --budget-ms 不见了（预算告警接入断掉）"
fi
grep -qF "junit_report.budget_from_suites(" "$RUNNER" \
    && ok "test_domain.py 仍调用 budget_from_suites" \
    || bad "test_domain.py 不再算预算（budget_from_suites 调用没了）"
grep -qF "junit_report.format_budget(" "$RUNNER" \
    && ok "test_domain.py 仍打印预算告警（format_budget）" \
    || bad "test_domain.py 不再打印预算告警（format_budget 调用没了）"
grep -qF "test_budget_gate.sh" "$ROOT/scripts/check.sh" \
    && ok "check.sh 仍挂载本闸" \
    || bad "check.sh 不再挂载本闸（预算告警坏了没人管）"

echo
if [ "$BAD" -ne 0 ]; then
    echo "==> test_budget_gate 未通过：$BAD 条断言失败（现场：$TMP）" >&2
    exit 1
fi
rm -rf "$TMP"
echo "==> test_budget_gate 通过"
exit 0
