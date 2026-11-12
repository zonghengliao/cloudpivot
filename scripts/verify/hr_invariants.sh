#!/usr/bin/env bash
# ============================================================================
# scripts/verify/hr_invariants.sh —— 人力资源模块数据一致性巡检（ARCH-9-2 §4.9）
#
# 卡：t_914199ef（HR-45）。断言实现见同目录 hr_invariants.sql（A1~A10 共 10 条，其中 8 条
# BLOCKER + A4/A5 两条 WARN；另有 W1/W2 两条 WARN 观察项）。
#
# 用法：
#   bash scripts/verify/hr_invariants.sh
#   HR_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/hr45_drill bash scripts/verify/hr_invariants.sh
#   NIK_DATABASE_NAME=cloudpivot_dev bash scripts/verify/hr_invariants.sh
#
# 退出码：
#   0 = 8 条 BLOCKER 零违规（A4/A5/W1/W2 是 WARN，只打印，不影响退出码）
#   1 = 有 BLOCKER 违规（明细已打印，且列出哪几条）
#   2 = 环境问题（psql 不存在 / 连不上 / 缺 HR 表 / SQL 语法错）
#
# 为什么不用 `psql -f` 的退出码直接判定：
#   巡检要「一次跑完 12 条、把该报的全报出来」，所以 SQL 里以 report_only 模式运行
#   （PGOPTIONS=-c hr.inv.report_only=1），**不**在第一条违规处中断；退出码由本脚本
#   读汇总行 `HR_INVARIANTS_SUMMARY blocker_fail=<n>` 决定。
#   裸跑 sql（不带本脚本）时默认是 fail-closed：有 BLOCKER 就 RAISE EXCEPTION。
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SQL="$HERE/hr_invariants.sql"
# shellcheck source=/dev/null
. "$HERE/hr/hr_db.sh"

command -v psql >/dev/null 2>&1 || { echo "FATAL: 没有 psql" >&2; exit 2; }
[ -f "$SQL" ] || { echo "FATAL: 找不到 $SQL" >&2; exit 2; }
hr_export_password

db="$(hr_db_name)"
echo "== HR 巡检（库 $db）=="

out="$(mktemp)"
trap 'rm -f "$out"' EXIT

# report_only=1：跑完 12 条再判定；client_min_messages=notice 保留 SQL 里的提示行
if ! PGOPTIONS='-c hr.inv.report_only=1' hr_psql -q -v ON_ERROR_STOP=1 -f "$SQL" >"$out" 2>&1; then
    echo "== 结果：环境/语法错误（不是违规，是巡检本身没跑起来）=="
    sed -n '1,40p' "$out"
    exit 2
fi

# 打印断言明细（去掉 psql 的空白填充，保留 `no level viol=… name`）
grep -E '^(A[0-9]+|W[0-9]+) ' "$out" | sed 's/[[:space:]]\+$//' || true
summary="$(grep -o 'HR_INVARIANTS_SUMMARY .*' "$out" | head -1)"
echo "$summary"

blocker="$(printf '%s' "$summary" | sed -nE 's/.*blocker_fail=([0-9]+).*/\1/p')"
warn="$(printf '%s' "$summary"    | sed -nE 's/.*warn_fail=([0-9]+).*/\1/p')"
blocker="${blocker:-0}"
warn="${warn:-0}"

if [ "$blocker" -gt 0 ]; then
    echo "== 结果：FAIL —— $blocker 条 BLOCKER 断言有违规（WARN 观察项 $warn 条）=="
    grep -E '^(A[0-9]+) .*viol=[1-9]' "$out" | sed -E 's/^/   ✗ /'
    exit 1
fi

echo "== 结果：PASS —— 8 条 BLOCKER 断言零违规（WARN 观察项 $warn 条，只报不拦）=="
exit 0
