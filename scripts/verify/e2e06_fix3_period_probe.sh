#!/usr/bin/env bash
# E2E06-FIX-3 实测探针（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）
#
# 验两件事（E2E-06 演练发现的两条 P4）：
#   ① 回填后：`over_budget=1 AND budget_id IS NULL` = 0 行；真超支（有 budget_id）不受影响；
#      回填前的原值留在 biz_data_fix_backup（fix_code=E2E06-FIX-3）；
#   ② 报销详情接口同时回「费用归属期间」与「记账期间」，跨期单据两者不同（逻辑不变，只明示）。
#
# 用法（145）：
#   bash scripts/verify/e2e06_fix3_period_probe.sh
# 退出码 = FAIL 数（0 = 全绿）。
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
# shellcheck source=scripts/verify/e2e06_budget_common.sh
. scripts/verify/e2e06_budget_common.sh

say "== E2E06-FIX-3 历史占用行回填 + 期间口径明示 =="
login_all

# ---------- ① 回填结果 ----------
say
say "① 回填：未编预算的占用行不得再标超支"
check "脏占用行（over=1 且 budget_id IS NULL）" \
  "$(SQ "select count(*) from biz_budget_usage where over_budget = 1 and budget_id is null and deleted_at is null")" "0"

TRUE_OVER=$(SQ "select count(*) from biz_budget_usage where over_budget = 1 and budget_id is not null and deleted_at is null")
if [ "${TRUE_OVER:-0}" -ge 1 ]; then
  ok "真超支占用行仍在（有 budget_id，over=1）：$TRUE_OVER 行"
else
  bad "真超支行消失（有 budget_id 且 over=1 的行数 = $TRUE_OVER）"
fi

BAK=$(SQ "select count(*) from biz_data_fix_backup where fix_code='E2E06-FIX-3'")
if [ "${BAK:-0}" -ge 1 ]; then
  ok "回填前原值已留档：$BAK 行（biz_data_fix_backup / fix_code=E2E06-FIX-3）"
else
  bad "没有回填留档（biz_data_fix_backup 里 fix_code=E2E06-FIX-3 为 0 行）"
fi

# 留档里的原值必须真的是「over=1」，否则说明留档抓错了行
BAK_BAD=$(SQ "select count(*) from biz_data_fix_backup where fix_code='E2E06-FIX-3' and (row_data->>'over_budget') <> '1'")
check "留档行原值 over_budget=1（抓错行 = 0）" "${BAK_BAD:-x}" "0"

# ---------- ② 期间口径（详情接口） ----------
say
say "② 报销详情：费用归属期间 vs 记账期间"

# 跨期单据（已入账、费用期间 ≠ 记账期间）
CROSS_ID=$(SQ "select id from biz_expense_report
                where deleted_at is null and voucher_id is not null
                  and to_char(expense_date,'YYYY-MM') <> period_code
                order by approved_at desc nulls last limit 1")
if [ -z "$CROSS_ID" ]; then
  bad "缺跨期样本（没有「已入账且费用期间≠记账期间」的报销单）"
else
  EXP_PERIOD=$(SQ "select to_char(expense_date,'YYYY-MM') from biz_expense_report where id='$CROSS_ID'::uuid")
  ACC_PERIOD=$(SQ "select period_code from biz_expense_report where id='$CROSS_ID'::uuid")
  api fix3_cross GET "/business/erp/expense/$CROSS_ID" "$T_ADMIN"
  check "详情接口 http" "$HTTP" "200"
  check "详情 expense_period_code（= 费用发生日所属月）" "$(data '.data.report.expense_period_code')" "$EXP_PERIOD"
  check "详情 accounting_period_code（= 凭证期间）" "$(data '.data.report.accounting_period_code')" "$ACC_PERIOD"
  checkne "跨期单据两个期间必须不同" "$EXP_PERIOD" "$ACC_PERIOD"
  # 预算占用按费用期间归集
  USAGE_PERIODS=$(SQ "select coalesce(string_agg(distinct period_code, ','), '-') from biz_budget_usage
                       where source_type='EXPENSE_REPORT' and source_id='$CROSS_ID'::uuid and deleted_at is null")
  check "占用期间 = 费用归属期间" "$USAGE_PERIODS" "$EXP_PERIOD"
  say "  · 样本 $(data '.data.report.report_no')：费用 $EXP_PERIOD / 记账 $ACC_PERIOD / 占用 $USAGE_PERIODS"
fi

# 未入账单据：记账期间必须为 null（前端显示「未记账」，不能伪造）
UNPOSTED_ID=$(SQ "select id from biz_expense_report where deleted_at is null and voucher_id is null
                   order by created_at desc limit 1")
if [ -n "$UNPOSTED_ID" ]; then
  api fix3_unposted GET "/business/erp/expense/$UNPOSTED_ID" "$T_ADMIN"
  check "未入账单 http" "$HTTP" "200"
  check "未入账单 accounting_period_code 必须为 null" "$(data '.data.report.accounting_period_code')" "null"
  check "未入账单 expense_period_code 仍有值" "$(data '.data.report.expense_period_code | length > 0')" "true"
fi

# ---------- ③ 历史脏单在详情里不再显示超支 ----------
say
say "③ 历史脏单（回填前 over=1 / 无预算）在详情里改为「未编制预算」"
LEGACY_ID=$(SQ "select b.row_id from biz_data_fix_backup b
                 where b.fix_code='E2E06-FIX-3' and b.table_name='biz_expense_report'
                 order by b.backed_up_at limit 1")
if [ -n "$LEGACY_ID" ]; then
  api fix3_legacy GET "/business/erp/expense/$LEGACY_ID" "$T_ADMIN"
  check "历史脏单 http" "$HTTP" "200"
  check "历史脏单 over_budget=false" "$(data '.data.report.over_budget')" "false"
  check "历史脏单 over_amount=null" "$(data '.data.report.over_amount')" "null"
  check "历史脏单 missing_budget=true" "$(data '.data.report.missing_budget')" "true"
  check "历史脏单占用行无超支标记" \
    "$(data '[.data.budget_usages[]|select(.over_budget)]|length')" "0"
  say "  · 样本 $(data '.data.report.report_no')"
fi

# ---------- ④ 报表接口仍可用（注脚是前端文案，接口不该被这卡改坏） ----------
say
say "④ 预算执行报表接口回归"
api fix3_report GET "/business/erp/budget/report?fiscal_year=2027" "$T_ADMIN"
check "报表 http" "$HTTP" "200"
check "报表 code" "$CODE" "0"

summary
exit "$FAIL"
