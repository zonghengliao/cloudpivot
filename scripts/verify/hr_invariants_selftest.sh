#!/usr/bin/env bash
# ============================================================================
# scripts/verify/hr_invariants_selftest.sh —— HR 巡检的「牙齿自检」（造错必红）
#
# 卡：t_914199ef（HR-45）。验收口径：10 条断言全部实现 / 故意造错能报红 / 全绿时零误报。
# 前两条由本脚本证明，第三条由本脚本的 R0（全绿基线）证明：
#   R0  夹具全套（员工 / 合同 / 标准 / 考勤 / 请假 / 已过账工资单 + 两张凭证）
#       ⇒ 12 条断言**全部 0 违规**（零误报）
#   I1~I10  只破坏第 N 条断言的单一前提，其他数据保持合法
#       ⇒ 断言 N 的 viol ≥ 1（有牙齿），且**每条断言都单独验证过**
#   F1  裸跑 sql（不带 report_only）时，有违规必须 psql 非 0 退出（fail-closed）
#
# 用法：
#   HR_DB_URL=postgres://user:pass@172.18.10.145:5432/hr45_drill bash scripts/verify/hr_invariants_selftest.sh
#
# 硬约束：
#   · 只许在**演练库**上跑（库名含 drill / e2e / test），护栏在 hr_db.sh 里，写在数据前面。
#   · 每轮都是「BEGIN → 灌夹具 → 造错 → 跑巡检 → ROLLBACK」：**零残留**，
#     跑完库里的业务表一行不多（夹具和注入全被回滚），可反复跑。
#
# 退出码：0 = 全绿 + 10 条全部报红 + fail-closed 成立；1 = 有不符合预期项；2 = 环境问题
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SQL="$HERE/hr_invariants.sql"
FIXTURE="$HERE/hr/hr_invariants_fixture.sql"
# shellcheck source=/dev/null
. "$HERE/hr/hr_db.sh"

command -v psql >/dev/null 2>&1 || { echo "FATAL: 没有 psql" >&2; exit 2; }
[ -f "$SQL" ]     || { echo "FATAL: 找不到 $SQL" >&2; exit 2; }
[ -f "$FIXTURE" ] || { echo "FATAL: 找不到 $FIXTURE" >&2; exit 2; }

hr_require_drill_db || exit 2   # ← 护栏必须在任何 DML 之前
hr_export_password

DB="$(hr_db_name)"
echo "== HR 巡检造错自检（库 $DB）=="
echo "   夹具：$FIXTURE"
echo "   巡检：$SQL"
echo

FAIL=0
TMP="$(mktemp)"; trap 'rm -f "$TMP"' EXIT

# run_round <名字> <注入 SQL 或空> [report_only]
#   输出整段 psql 结果（含 12 行断言明细 + 汇总行）
run_round() {
    local script; script="$(mktemp)"
    {
        echo 'BEGIN;'
        printf '\\i %s\n' "$FIXTURE"
        [ -n "${2:-}" ] && printf '%s\n' "$2"
        printf '\\i %s\n' "$SQL"
        echo 'ROLLBACK;'
    } >"$script"

    local opts='-c hr.inv.report_only=1'
    [ "${3:-report_only}" = "fail_closed" ] && opts=''
    PGOPTIONS="$opts" hr_psql -q -v ON_ERROR_STOP=1 -f "$script" 2>&1
    local rc=$?
    rm -f "$script"
    return $rc
}

# 取某条断言的违规数（拿不到 ⇒ 输出空）
viol_of() {
    printf '%s' "$1" | awk -v k="$2" '$1 == k {
        for (i = 1; i <= NF; i++) if ($i ~ /^viol=/) { sub(/^viol=/, "", $i); print $i; exit }
    }'
}

# 断言「输出里有全部 12 条断言行」——防止某条断言被静默删掉还判绿
check_all_present() {
    local out="$1" tag="$2" missing=""
    local k
    for k in A1 A2 A3 A4 A5 A6 A7 A8 A9 A10 W1 W2; do
        printf '%s' "$out" | grep -q "^$k " || missing="$missing $k"
    done
    if [ -n "$missing" ]; then
        echo "   ✗ [$tag] 巡检输出缺少断言行：$missing（断言被删或 SQL 提前中断）"
        return 1
    fi
    return 0
}

round() {  # round <tag> <注入 SQL>
    run_round "$1" "$2" >"$TMP" 2>&1
}

# ---------------------------------------------------------------------------
# R0：全绿基线 —— 零误报
# ---------------------------------------------------------------------------
echo "── R0 全绿基线（夹具齐全，什么都不造错）"
if ! round R0 ""; then
    echo "   ✗ R0 巡检本身执行失败（不是违规，是 SQL/夹具跑不起来）："
    sed -n '1,30p' "$TMP" | sed 's/^/     /'
    exit 2
fi
check_all_present "$(cat "$TMP")" R0 || FAIL=$((FAIL + 1))
grep -E '^(A[0-9]+|W[0-9]+) ' "$TMP" | sed 's/[[:space:]]\+$//;s/^/   /'
grep -o 'HR_INVARIANTS_SUMMARY .*' "$TMP" | sed 's/^/   /'
r0_blocker="$(grep -o 'blocker_fail=[0-9]*' "$TMP" | head -1 | cut -d= -f2)"
r0_warn="$(grep -o 'warn_fail=[0-9]*' "$TMP" | head -1 | cut -d= -f2)"
if [ "${r0_blocker:-1}" != "0" ]; then
    echo "   ✗ R0 有误报：blocker_fail=${r0_blocker:-?}（全绿基线不许有任何 BLOCKER 违规）"
    FAIL=$((FAIL + 1))
else
    echo "   ✓ R0 零误报：8 条 BLOCKER 全 0（WARN 观察项 ${r0_warn:-?} 条，含 A4/A5）"
fi
echo

# ---------------------------------------------------------------------------
# I1 ~ I10：逐条造错，验证对应断言报红
# ---------------------------------------------------------------------------
declare -a TAGS TARGETS NAMES MUTS
TAGS=(I1 I2 I3 I4 I5 I6 I7 I8 I9 I10)
TARGETS=(A1 A2 A3 A4 A5 A6 A7 A8 A9 A10)
NAMES=(
  "A1：员工档挂到不存在的账号"
  "A2：同一 user_id 挂两份员工档（先临时 drop 库层唯一索引，验证巡检能独立发现）"
  "A3：同一员工同一薪资项追加一段重叠区间"
  "A4：把唯一一份有效合同软删（在职员工无合同）"
  "A5：员工改成离职且离职日晚于合同到期日（合同不该在离职后到期）"
  "A6：明细 gross 加 100 而单头不动 → 单头 ≠ Σ 明细"
  "A7：明细项 BASE 加 50 而明细汇总不动 → 明细项 ≠ 明细汇总"
  "A8：已过账单据的发放凭证指针清空"
  "A9：计提凭证挪到 1999-01（期间表里没有这个期间）"
  "A10：请假单改 APPROVED 但实例仍 PENDING"
)
MUTS=(
  "UPDATE biz_employee SET user_id = '00000000-0000-4000-8000-0000000c4999' WHERE emp_no = 'HR45-001';"
  $'DROP INDEX IF EXISTS uk_biz_employee_user;\nINSERT INTO biz_employee (id, org_id, emp_no, name, dept_id, hire_date, status, employment_type, user_id)\nSELECT \'00000000-0000-4000-8000-0000000c4531\'::uuid, org_id, \'HR45-DUP\', name, dept_id, hire_date, \'ACTIVE\', employment_type, user_id\n  FROM biz_employee WHERE emp_no = \'HR45-001\';'
  "INSERT INTO biz_employee_salary (id, org_id, employee_id, item_code, amount, effective_from, effective_to)
SELECT '00000000-0000-4000-8000-0000000c4532'::uuid, org_id, id, 'BASE', 12000,
       CURRENT_DATE - INTERVAL '1 month', CURRENT_DATE + INTERVAL '1 month'
  FROM biz_employee WHERE emp_no = 'HR45-001';"
  "UPDATE biz_employee_contract SET deleted_at = now() WHERE contract_no = 'HT-HR45-001';"
  "UPDATE biz_employee SET status = 'RESIGNED', leave_type = 'RESIGN',
          leave_date = CURRENT_DATE + INTERVAL '3 year'
    WHERE emp_no = 'HR45-001';"
  "UPDATE biz_salary_detail
      SET earning_amount = earning_amount + 100,
          gross_amount   = gross_amount + 100,
          net_amount     = net_amount + 100
    WHERE employee_no = 'HR45-001';"
  "UPDATE biz_salary_detail_item SET amount = amount + 50 WHERE item_code = 'BASE';"
  "UPDATE biz_salary_sheet SET payment_voucher_id = NULL WHERE sheet_no = 'SAL-HR45-0001';"
  "UPDATE biz_voucher SET period = '1999-01' WHERE voucher_no = '记-HR45-ACC';"
  "UPDATE biz_leave_request SET status = 'APPROVED' WHERE leave_no = 'LV-HR45-0001';"
)

for i in "${!TAGS[@]}"; do
    tag="${TAGS[$i]}"
    target="${TARGETS[$i]}"
    echo "── $tag ${NAMES[$i]}（目标断言 $target）"
    if ! round "$tag" "${MUTS[$i]}"; then
        echo "   ✗ $tag 巡检执行失败（注入 SQL 或巡检 SQL 报错）："
        sed -n '1,20p' "$TMP" | sed 's/^/     /'
        FAIL=$((FAIL + 1)); echo; continue
    fi
    check_all_present "$(cat "$TMP")" "$tag" || FAIL=$((FAIL + 1))
    v="$(viol_of "$(cat "$TMP")" "$target")"
    echo "   $target viol=${v:-?} ｜ $(grep -E "^$target " "$TMP" | sed 's/[[:space:]]\+$//' | sed 's/^.*viol=[0-9]*[[:space:]]*//' | cut -c1-90)"
    if [ -z "$v" ] || [ "$v" -lt 1 ]; then
        echo "   ✗ $tag 没报红：$target 的 viol=${v:-（缺行）}，断言没牙"
        FAIL=$((FAIL + 1))
    else
        echo "   ✓ $tag 报红（$target viol=$v）"
    fi
    echo
done

# ---------------------------------------------------------------------------
# F1：裸跑（不带 report_only）必须 fail-closed —— 有 BLOCKER 就非 0 退出
# ---------------------------------------------------------------------------
echo "── F1 fail-closed：裸跑 sql（无 report_only）有违规必须非 0 退出"
run_round F1 "${MUTS[7]}" fail_closed >"$TMP" 2>&1
rc=$?
if [ "$rc" -eq 0 ]; then
    echo "   ✗ F1 裸跑在有违规时仍返回 0（巡检会假绿）"
    FAIL=$((FAIL + 1))
elif ! grep -q 'HR 巡检失败' "$TMP"; then
    echo "   ✗ F1 非 0 退出但不是巡检 RAISE（rc=$rc，先看输出）"
    sed -n '1,20p' "$TMP" | sed 's/^/     /'
    FAIL=$((FAIL + 1))
else
    echo "   ✓ F1 裸跑 fail-closed（rc=$rc，RAISE 已触发）"
fi
echo

if [ "$FAIL" -gt 0 ]; then
    echo "== 结果：FAIL —— $FAIL 项不符合预期"
    exit 1
fi
echo "== 结果：PASS —— R0 零误报 + I1~I10 十条断言全部能报红 + F1 fail-closed 成立"
exit 0
