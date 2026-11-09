#!/usr/bin/env bash
# E2E06-FIX-2 报表口径实测（145 测试机 · cloudpivot_dev · 真后端 :8033）
#
# 验证两件事（都是 E2E-06 演练发现的 P3）：
#   ① 执行报表主列带上**月度口径**：`month_over / month_over_amount / month_available / month_period`，
#      且与提交侧月度闸（`budget_guard::MonthGate`）同一套公式；
#   ② `effective` 以**该行 fiscal_year 的期末**判定，不再用「今天/本月月末」。
#
# 用法（145）：
#   bash scripts/verify/e2e06_fix2_report_probe.sh            # 追加断言到 /tmp/e2e06/asserts.log
#   OUT=... bash ...                                          # 只写文件
#
# 依赖 e2e06_budget_a.sh / e2e06_budget_b.sh 留下的夹具（同一部门 E2E06-BDG）：
#   · 5602.05 通讯费 FY2027：月额度 1000，2027-02 报了 1500（WARN 槽，年额度 12000）
#   · 5602.02 差旅费 FY2027：年额度 150000（effective_from = 审批次月 1 日）
#   · 5602.02 差旅费 FY2023：effective_from 晚于 2023 年末（未到生效日）
# 没跑过 A/B 时本脚本会明确报缺夹具，而不是给出假绿。
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=scripts/verify/e2e06_budget_common.sh
. scripts/verify/e2e06_budget_common.sh

say "== E2E06-FIX-2 执行报表口径（月度 + effective 时点） =="
login_all

DEPT=$(SQ "select id from sys_dept where dept_code='E2E06-BDG' and deleted_at is null")
[ -n "$DEPT" ] || { say "FATAL: 缺夹具部门 E2E06-BDG（先跑 e2e06_budget_a.sh）"; exit 1; }
ACC_COMM=$(acc '5602.05')
ACC_TRAVEL=$(acc '5602.02')
[ -n "$ACC_COMM" ] && [ -n "$ACC_TRAVEL" ] || { say "FATAL: 缺科目夹具 5602.05 / 5602.02"; exit 1; }

# 报表行定位：按「年度 + 部门 + 科目」找，不依赖上一轮生成的 budget_id
row_of() { # row_of <fiscal_year> <account_code>
  printf '%s' "$BODY" | jq -c --arg y "$1" --arg a "$2" --arg d "$DEPT" \
    '[.data[]|select(.fiscal_year==($y|tonumber) and .account_code==$a and .dept_id==$d)][0]'
}
fld() { printf '%s' "$1" | jq -r "$2"; }

say
say "① FY2027 执行报表：5602.05（月额度 1000，2027-02 报 1500）"
api fx2_rep27 GET "/business/erp/budget/report?fiscal_year=2027" "$T_ADMIN"
check "报表 http" "$HTTP" "200"
check "报表 code" "$CODE" "0"
ROW_W=$(row_of 2027 '5602.05')
if [ -z "$ROW_W" ] || [ "$ROW_W" = "null" ]; then
  bad "缺夹具：FY2027 的 5602.05 槽不在报表里（先跑 e2e06_budget_b.sh）"
  summary; exit 1
fi
say "  · 行：$(printf '%s' "$ROW_W" | jq -c '{total_amount,used,available,over,month_period,month_limit,month_used,month_available,month_over,month_over_amount,effective}')"

# ---- 验收 ①：月度超 500 ----
check "month_over=true（本月已超，主列要能看出来）" "$(fld "$ROW_W" '.month_over')" "true"
chknum "month_over_amount = 500" "$(fld "$ROW_W" '.month_over_amount')" "500"
chknum "month_available = 1000 − 1500 = −500" "$(fld "$ROW_W" '.month_available')" "-500"
check "month_period = 2027-02（超支的那个月）" "$(fld "$ROW_W" '.month_period')" "2027-02"
chknum "month_limit = 1000" "$(fld "$ROW_W" '.month_limit')" "1000"
chknum "month_used = 1500" "$(fld "$ROW_W" '.month_used')" "1500"

# ---- 验收 ②：年度没超（保持年度口径），主列同时给月度 ----
check "over=false（年度口径不变）" "$(fld "$ROW_W" '.over')" "false"
chknum "available = 12000 − 1500 = 10500（年度）" "$(fld "$ROW_W" '.available')" "10500"

# ---- ⑤ 与提交侧同源：报表的月度超支额 == 提交时冻结的 over_amount ----
DB_OVER=$(SQ "select coalesce(max(over_amount),0)::text from biz_budget_usage where dept_id='$DEPT'::uuid and account_id='$ACC_COMM'::uuid and fiscal_year=2027 and over_budget = 1 and deleted_at is null")
chknum "报表 month_over_amount == 提交侧冻结的 over_amount（DB=$DB_OVER）" \
  "$(fld "$ROW_W" '.month_over_amount')" "$DB_OVER"
# 分月明细仍在（不是把 months 换成月度口径，是**加**了一列）
chknum "分月明细 2027-02 可用仍为 −500" \
  "$(printf '%s' "$ROW_W" | jq -r '[.months[]|select(.period_code=="2027-02")][0].available')" "-500"

say
say "② 月度未超的槽不能误报（5602.09 其他费用，FY2027，零发生）"
ROW_B=$(row_of 2027 '5602.09')
if [ -n "$ROW_B" ] && [ "$ROW_B" != "null" ]; then
  check "month_over=false（没超就不报）" "$(fld "$ROW_B" '.month_over')" "false"
  check "month_over_amount=null（不是 0）" "$(fld "$ROW_B" '.month_over_amount')" "null"
else
  say "  · 跳过：FY2027 的 5602.09 槽不在报表里（BLOCK 槽，b.sh 未建则跳过）"
fi

say
say "③ FY2027 预算（effective_from=审批次月 1 日）→ 报表 fiscal_year=2027 时 effective=true"
ROW_A=$(row_of 2027 '5602.02')
if [ -n "$ROW_A" ] && [ "$ROW_A" != "null" ]; then
  EF=$(fld "$ROW_A" '.effective_from')
  check "effective=true（修复前按「本月月末」判 → false，与 2027-02 已被控制自相矛盾）" \
    "$(fld "$ROW_A" '.effective')" "true"
  check "effective_from 原样回显（$EF）" "$(fld "$ROW_A" '.effective_from')" "$EF"
else
  say "  · 跳过：FY2027 的 5602.02 槽不在报表里"
fi

say
say "④ 年度早于生效日（FY2023 预算，effective_from=2026-12-01 > 2023-12-31）→ effective=false"
api fx2_rep23 GET "/business/erp/budget/report?fiscal_year=2023" "$T_ADMIN"
check "FY2023 报表 http" "$HTTP" "200"
ROW_E=$(row_of 2023 '5602.02')
if [ -n "$ROW_E" ] && [ "$ROW_E" != "null" ]; then
  EF_E=$(fld "$ROW_E" '.effective_from')
  check "effective=false（生效日晚于该年度期末）" "$(fld "$ROW_E" '.effective')" "false"
  say "  · effective_from=$EF_E"
else
  say "  · 跳过：FY2023 的 5602.02 槽不在报表里（a.sh 未建则跳过）"
fi

summary
