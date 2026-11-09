#!/usr/bin/env bash
# E2E-06 阶段 B：④ 我的报销全流程（建单→票据→提交→撤回→审批→付款→作废）
#                超支拦截（BLOCK 409 / WARN 放行标记）
#                ⑥ 预算执行（已用/可用/执行率）+ 预算详情 usages[]
# 依赖阶段 A 已跑（BG_A2 = 5602.02 差旅费 ACTIVE 15 万；BG_B = 5602.09 BLOCK 1.2 万）
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$DIR/e2e06_budget_common.sh"

say "==================== E2E-06 B：报销全流程 + 超支拦截 + 预算执行 ===================="
login_all
login_rest
[ -z "${BG_A2:-}" ] && { say "FATAL: 缺 BG_A2，请先跑 e2e06_budget_a.sh"; exit 1; }

ACC_TRAVEL=$(acc 5602.02); ACC_OTHER=$(acc 5602.09); ACC_COMM=$(acc 5602.05)
TYPE_ID=$(SQ "select id from biz_expense_type where type_code='E2E06_TRAVEL' and deleted_at is null")
say "  · 费用类型 E2E06_TRAVEL=$TYPE_ID（科目 5602.02）"

# ============ ④ 我的报销 ============
say
say "④-1 建单（含税录入 → 服务端拆价税）"
api ex1 POST /business/erp/expense "$T_CLERK" \
  "{\"title\":\"E2E06 客户拜访差旅\",\"expense_date\":\"2027-02-12\",\"dept_id\":\"$DEPT_ID\",\"items\":[{\"expense_type_id\":\"$TYPE_ID\",\"with_tax_amount\":\"3180\",\"tax_rate\":\"0.06\",\"summary\":\"高铁+住宿（客户拜访）\",\"invoice_no\":\"INV-E2E06-101\",\"invoice_type\":\"VAT_SPECIAL\"}],\"remark\":\"E2E-06 主链报销单\"}"
check "建单 http" "$HTTP" "200"
EXP1=$(data '.data.report.id'); save EXP1 "$EXP1"
NO1=$(data '.data.report.report_no'); save NO1 "$NO1"
check "单号形态 EXP-" "$(printf '%s' "$NO1" | cut -c1-4)" "EXP-"
check "价税/税额/不含税 = 3180/180/3000" \
  "$(SQ "select total_with_tax::text||'|'||total_tax::text||'|'||total_amount::text from biz_expense_report where id='$EXP1'::uuid")" \
  "3180.0000|180.0000|3000.0000"
check "状态 DRAFT + 未付款" "$(SQ "select status||'|'||pay_status from biz_expense_report where id='$EXP1'::uuid")" "DRAFT|UNPAID"
check "明细科目快照 5602.02" "$(SQ "select account_code from biz_expense_item where report_id='$EXP1'::uuid")" "5602.02"

say "  · 无附件提交 → 400（ATT-06 发票影像必传）"
api ex1_nofile POST "/business/erp/expense/$EXP1/submit" "$T_CLERK" '{}'
check "缺附件提交 $HTTP" "$HTTP" "400"
check "报文点名缺附件" "$(printf '%s' "$BODY" | jq -r '.msg' | grep -c '发票影像')" "1"
exp_att "$EXP1" exp1

say "④-2 提交审批（占用预算 OCCUPIED）"
api ex1_sub POST "/business/erp/expense/$EXP1/submit" "$T_CLERK" '{}'
check "提交 http" "$HTTP" "200"
check "单据 SUBMITTED" "$(SQ "select status from biz_expense_report where id='$EXP1'::uuid")" "SUBMITTED"
check "占用落库（OCCUPIED / 3180 / 不超支 / 期间 2027-02）" \
  "$(SQ "select status||'|'||amount::text||'|'||over_budget||'|'||period_code from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP1'::uuid")" \
  "OCCUPIED|3180.0000|0|2027-02"
check "单据预算策略快照 WARN" "$(SQ "select budget_mode from biz_expense_report where id='$EXP1'::uuid")" "WARN"

say "  · FIX-2 回归：预算详情 usages[] 能看到报销产生的占用"
api bg_det GET "/business/erp/budget/$BG_A2" "$T_APPR"
check "预算详情 http" "$HTTP" "200"
check "usages[] 里出现本单（source_no）" \
  "$(printf '%s' "$BODY" | jq -r --arg no "$NO1" '[.data.usages[]|select(.source_no==$no)]|length')" "1"
check "usages[] 行 状态/金额/超支标记" \
  "$(printf '%s' "$BODY" | jq -r --arg no "$NO1" '[.data.usages[]|select(.source_no==$no)|"\(.status)|\(.amount)|\(.over_budget)|\(.source_type)"]|join(",")')" \
  "OCCUPIED|3180.0000|false|EXPENSE_REPORT"
check "预算年度已用 = 3180" "$(printf '%s' "$BODY" | jq -r '.data.year_used')" "3180.0000"
MONTH_USED=$(printf '%s' "$BODY" | jq -r '[.data.periods[]|select(.period_code=="2027-02")][0].used')
check "月度分解 2027-02 已用 = 3180" "$MONTH_USED" "3180.0000"
MONTH_AVAIL=$(printf '%s' "$BODY" | jq -r '[.data.periods[]|select(.period_code=="2027-02")][0].available')
check "月度分解 2027-02 可用 = 12500−3180" "$MONTH_AVAIL" "9320.0000"

say "④-3 撤回（释放占用）→ 重提（复用同一占用行）"
UID_BEFORE=$(SQ "select id from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP1'::uuid")
api ex1_wd POST "/business/erp/expense/$EXP1/withdraw" "$T_CLERK" '{}'
check "撤回 http" "$HTTP" "200"
check "撤回后 DRAFT + 占用 RELEASED（带释放原因）" \
  "$(SQ "select (select status from biz_expense_report where id='$EXP1'::uuid)||'|'||status||'|'||(release_reason is not null) from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP1'::uuid")" \
  "DRAFT|RELEASED|true"
api ex1_re POST "/business/erp/expense/$EXP1/submit" "$T_CLERK" '{}'
check "重提 http" "$HTTP" "200"
check "重提复用同一占用行（不新增行）" \
  "$(SQ "select count(*) from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP1'::uuid")" "1"
check "复用行 id 不变 + 状态回 OCCUPIED" \
  "$(SQ "select id::text||'|'||status from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP1'::uuid")" "$UID_BEFORE|OCCUPIED"

say "④-4 审批（直属上级 → 财务经理）→ 入账 + 占用转已用"
api ex1_self POST "/business/flow/task/$(tasks_of EXPENSE "$EXP1" PENDING | head -1 | cut -d'|' -f1)/approve" "$T_CLERK" '{"comment":"自己批自己"}'
check "报销人自批被内控拒绝 $HTTP" "$HTTP" "403"
expense_approve "$EXP1" ex1ap
check "实例 APPROVED" "$(SQ "select status from biz_flow_instance where biz_type='EXPENSE' and biz_id='$EXP1'::uuid order by created_at desc limit 1")" "APPROVED"
check "单据 APPROVED + 计提凭证 + 入账期间 2026-11" \
  "$(SQ "select status||'|'||(voucher_id is not null)||'|'||period_code from biz_expense_report where id='$EXP1'::uuid")" \
  "APPROVED|true|2026-11"
V1=$(SQ "select voucher_id from biz_expense_report where id='$EXP1'::uuid"); save V1 "$V1"
check "计提凭证分录（借5602.02 3000 + 借2221.01 180 / 贷2241.01 3180）" \
  "$(SQ "select string_agg(account_code||':'||debit_amount::text||'/'||credit_amount::text,',' order by line_no) from biz_voucher_entry where voucher_id='$V1'::uuid")" \
  "5602.02:3000.0000/0.0000,2221.01:180.0000/0.0000,2241.01:0.0000/3180.0000"
check "凭证借贷平衡" \
  "$(SQ "select (total_debit=total_credit) from biz_voucher where id='$V1'::uuid")" "t"
check "占用转 CONSUMED + 绑凭证" \
  "$(SQ "select status||'|'||(voucher_id='$V1'::uuid) from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP1'::uuid")" "CONSUMED|true"

say "⑤ 待付款（已审批未付清）→ 付款"
api pay_page GET "/business/erp/expense/pay/page?page=1&page_size=50" "$T_ADMIN"
check "待付款分页 http" "$HTTP" "200"
check "待付款列表含本单" "$(printf '%s' "$BODY" | jq -r --arg id "$EXP1" '[.data.list[]|select(.id==$id)]|length')" "1"
api pay1 POST "/business/erp/expense/$EXP1/pay" "$T_ADMIN" \
  '{"amount":"1000","pay_method":"BANK_TRANSFER","pay_account_code":"1002","remark":"E2E-06 部分付款"}'
check "部分付款 http" "$HTTP" "200"
check "付款进度 PARTIAL / 已付 1000 / 未付 2180" \
  "$(SQ "select pay_status||'|'||paid_amount::text||'|'||unpaid_amount::text from biz_expense_report where id='$EXP1'::uuid")" \
  "PARTIAL|1000.0000|2180.0000"
PV1=$(SQ "select id from biz_expense_payment where report_id='$EXP1'::uuid order by created_at limit 1")
check "付款凭证（借2241.01 1000 / 贷1002 1000）" \
  "$(SQ "select string_agg(account_code||':'||debit_amount::text||'/'||credit_amount::text,',' order by line_no) from biz_voucher_entry where voucher_id=(select voucher_id from biz_expense_payment where id='$PV1'::uuid)")" \
  "2241.01:1000.0000/0.0000,1002:0.0000/1000.0000"
api pay2 POST "/business/erp/expense/$EXP1/pay" "$T_ADMIN" \
  '{"amount":"2180","pay_method":"BANK_TRANSFER","pay_account_code":"1002","remark":"E2E-06 付清尾款"}'
check "付清 http" "$HTTP" "200"
check "付清后 PAID / 未付 0" \
  "$(SQ "select pay_status||'|'||unpaid_amount::text from biz_expense_report where id='$EXP1'::uuid")" "PAID|0.0000"
api pay3 POST "/business/erp/expense/$EXP1/pay" "$T_ADMIN" '{"amount":"1"}'
check "超额付款被拒 $HTTP" "$HTTP" "409"

say "④-5 作废（已审批未付清的另建一单 → 红冲 + 释放占用）"
api ex2 POST /business/erp/expense "$T_CLERK" \
  "{\"title\":\"E2E06 待作废的报销\",\"expense_date\":\"2027-02-15\",\"dept_id\":\"$DEPT_ID\",\"items\":[{\"expense_type_id\":\"$TYPE_ID\",\"with_tax_amount\":\"500\",\"tax_rate\":\"0.06\",\"summary\":\"市内交通\",\"invoice_no\":\"INV-E2E06-102\",\"invoice_type\":\"VAT_NORMAL\"}]}"
check "建单（作废用）http" "$HTTP" "200"
EXP2=$(data '.data.report.id'); save EXP2 "$EXP2"
exp_att "$EXP2" exp2 >/dev/null
api ex2_sub POST "/business/erp/expense/$EXP2/submit" "$T_CLERK" '{}'
check "提交（作废用）http" "$HTTP" "200"
expense_approve "$EXP2" ex2ap
check "作废前已入账" "$(SQ "select status||'|'||(voucher_id is not null) from biz_expense_report where id='$EXP2'::uuid")" "APPROVED|true"
api ex2_bad POST "/business/erp/expense/$EXP2/cancel" "$T_ADMIN" '{"reason":"太短"}'
check "作废原因过短被拒 $HTTP" "$HTTP" "400"
api ex2_cancel POST "/business/erp/expense/$EXP2/cancel" "$T_ADMIN" '{"reason":"E2E-06 演练：重复报销，作废本单（原因≥10字）"}'
check "作废 http" "$HTTP" "200"
check "单据 CANCELLED" "$(SQ "select status from biz_expense_report where id='$EXP2'::uuid")" "CANCELLED"
check "红冲凭证 is_red=1 且指向原凭证" \
  "$(SQ "select count(*) from biz_voucher v where v.source_type='EXPENSE_REPORT' and v.source_id='$EXP2'::uuid and v.is_red=1")" "1"
check "占用释放（RELEASED）" \
  "$(SQ "select status from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP2'::uuid")" "RELEASED"
chknum "作废后 2241.01 对本单净额归零（计提+红冲）" \
  "$(SQ "select coalesce(sum(e.debit_amount-e.credit_amount),0)::text from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id where v.source_type='EXPENSE_REPORT' and v.source_id='$EXP2'::uuid and e.account_code='2241.01'")" \
  "0"

# ============ 超支拦截 ============
say
say "④-6 BLOCK 槽（5602.09，月额度 1000）超支 → 409，报文说清科目/已用/预算/超支"
api ex_block POST /business/erp/expense "$T_CLERK" \
  "{\"title\":\"E2E06 BLOCK 超支\",\"expense_date\":\"2027-02-16\",\"dept_id\":\"$DEPT_ID\",\"items\":[{\"expense_type_id\":\"$(SQ "select id from biz_expense_type where type_code='OTHER' and deleted_at is null")\",\"with_tax_amount\":\"1500\",\"tax_rate\":\"0.06\",\"summary\":\"其他费用超支（BLOCK）\",\"invoice_no\":\"INV-E2E06-103\",\"invoice_type\":\"VAT_NORMAL\"}]}"
check "建单（BLOCK）http" "$HTTP" "200"
EXP3=$(data '.data.report.id'); save EXP3 "$EXP3"
exp_att "$EXP3" exp3 >/dev/null
api ex_block_sub POST "/business/erp/expense/$EXP3/submit" "$T_CLERK" '{}'
check "BLOCK 超支提交 $HTTP" "$HTTP" "409"
BMSG=$(printf '%s' "$BODY" | jq -r '.msg')
save BMSG "$BMSG"
check "报文含科目编码 5602.09" "$(printf '%s' "$BMSG" | grep -c '5602.09')" "1"
check "报文含「月度已用/预算/本次/超支」四项数字" \
  "$(printf '%s' "$BMSG" | grep -c '月度已用 .* / 月度预算 .*，本次 .*，超支 ')" "1"
check "报文点名阻断策略来自槽级" "$(printf '%s' "$BMSG" | grep -c '槽级策略 BLOCK')" "1"
check "被拦后单据仍 DRAFT" "$(SQ "select status from biz_expense_report where id='$EXP3'::uuid")" "DRAFT"
check "被拦后不留占用行（事务回滚）" \
  "$(SQ "select count(*) from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP3'::uuid")" "0"
check "被拦后不留审批实例" \
  "$(SQ "select count(*) from biz_flow_instance where biz_type='EXPENSE' and biz_id='$EXP3'::uuid")" "0"

say "④-7 WARN 槽（5602.05 通讯费，月额度 1000）超支 → 放行 + 标记"
api bg_warn POST /business/erp/budget "$T_P3A" \
  "{\"fiscal_year\":2027,\"dept_id\":\"$DEPT_ID\",\"account_id\":\"$ACC_COMM\",\"total_amount\":\"12000\",\"periods\":$(jq -nc '[range(1;13)|{month:.,amount:"1000"}]'),\"remark\":\"E2E-06 通讯费（WARN 继承全局）\"}"
check "建 WARN 槽预算 http" "$HTTP" "200"
BG_W=$(data '.data.budget.id'); save BG_W "$BG_W"
api bg_warn_sub POST "/business/erp/budget/$BG_W/submit" "$T_P3A" '{}'
budget_active "$BG_W" bgW
check "WARN 槽 ACTIVE" "$(SQ "select status from biz_budget where id='$BG_W'::uuid")" "ACTIVE"
api ex_warn POST /business/erp/expense "$T_CLERK" \
  "{\"title\":\"E2E06 WARN 超支\",\"expense_date\":\"2027-02-18\",\"dept_id\":\"$DEPT_ID\",\"items\":[{\"expense_type_id\":\"$(SQ "select id from biz_expense_type where type_code='COMM' and deleted_at is null")\",\"with_tax_amount\":\"1500\",\"tax_rate\":\"0.06\",\"summary\":\"通讯费超月度额度（WARN）\",\"invoice_no\":\"INV-E2E06-104\",\"invoice_type\":\"VAT_NORMAL\"}]}"
check "建单（WARN）http" "$HTTP" "200"
EXP4=$(data '.data.report.id'); save EXP4 "$EXP4"
exp_att "$EXP4" exp4 >/dev/null
api ex_warn_sub POST "/business/erp/expense/$EXP4/submit" "$T_CLERK" '{}'
check "WARN 超支仍可提交 $HTTP" "$HTTP" "200"
check "单据标超支 over_budget=1 / over_amount=500（月缺口）" \
  "$(SQ "select over_budget::text||'|'||over_amount::text from biz_expense_report where id='$EXP4'::uuid")" \
  "1|500.0000"
check "占用行同步标超支" \
  "$(SQ "select over_budget::text||'|'||over_amount::text from biz_budget_usage where source_type='EXPENSE_REPORT' and source_id='$EXP4'::uuid")" \
  "1|500.0000"
expense_approve "$EXP4" ex4ap
check "WARN 单照常入账（审批通过后可执行）" \
  "$(SQ "select status from biz_expense_report where id='$EXP4'::uuid")" "APPROVED"

# ============ ⑥ 预算执行 ============
say
say "⑥ 预算执行报表（已用 / 可用 / 执行率）"
api rep GET "/business/erp/budget/report?fiscal_year=2027" "$T_APPR"
check "执行报表 http" "$HTTP" "200"
check "报表 code" "$CODE" "0"
ROW_A=$(printf '%s' "$BODY" | jq -c '[.data[]|select(.budget_id=="'"$BG_A2"'")][0]')
say "  · 5602.02 行：$(printf '%s' "$ROW_A" | jq -c '{total_amount,occupied,consumed,used,available,exec_ratio,over,effective}')"
USED_DB=$(SQ "select coalesce(sum(amount),0)::text from biz_budget_usage where dept_id='$DEPT_ID'::uuid and account_id='$ACC_TRAVEL'::uuid and fiscal_year=2027 and status in ('OCCUPIED','CONSUMED') and deleted_at is null")
chknum "报表已用 = 库内占用合计" "$(printf '%s' "$ROW_A" | jq -r '.used')" "$USED_DB"
chknum "报表可用 = 额度 − 已用" "$(printf '%s' "$ROW_A" | jq -r '.available')" "$(awk -v t=150000 -v u="$USED_DB" 'BEGIN{printf "%.4f", t-u}')"
check "报表 effective=true（effective_from=2026-12-01 ≤ FY2027 期末 2027-12-31；修复前按本月月末判成 false）" "$(printf '%s' "$ROW_A" | jq -r '.effective')" "true"
check "报表 effective_from = 2026-12-01" "$(printf '%s' "$ROW_A" | jq -r '.effective_from')" "2026-12-01"
check "报表 over=false（年度未超支）" "$(printf '%s' "$ROW_A" | jq -r '.over')" "false"
ROW_W=$(printf '%s' "$BODY" | jq -c '[.data[]|select(.budget_id=="'"$BG_W"'")][0]')
check "WARN 槽报表 over=false（报表 over 是年度口径）" "$(printf '%s' "$ROW_W" | jq -r '.over')" "false"
# E2E06-FIX-2：同一行的**月度口径**（此前只藏在 months[] 里，主列看不出来）
check "WARN 槽 month_over=true（本月已超，与提交侧月度闸同源）" "$(printf '%s' "$ROW_W" | jq -r '.month_over')" "true"
chknum "WARN 槽 month_over_amount = 500" "$(printf '%s' "$ROW_W" | jq -r '.month_over_amount')" "500"
chknum "WARN 槽 month_available = −500" "$(printf '%s' "$ROW_W" | jq -r '.month_available')" "-500"
check "WARN 槽 month_period = 2027-02" "$(printf '%s' "$ROW_W" | jq -r '.month_period')" "2027-02"
check "5602.02 槽 month_over=false（月度没超，不许误报）" "$(printf '%s' "$ROW_A" | jq -r '.month_over')" "false"
chknum "WARN 槽 2027-02 月度可用 = 1000−1500 = −500（月度超支在分月里）" \
  "$(printf '%s' "$ROW_W" | jq -r '[.months[]|select(.period_code=="2027-02")][0].available')" "-500"
chknum "WARN 槽年度可用 = 12000−1500" "$(printf '%s' "$ROW_W" | jq -r '.available')" "10500"
EXP_RATIO=$(printf '%s' "$ROW_W" | jq -r '.exec_ratio')
chknum "WARN 槽执行率 = 1500/12000×100" "$EXP_RATIO" "$(awk 'BEGIN{printf "%.2f", 1500/12000*100}')"
ROW_B=$(printf '%s' "$BODY" | jq -c '[.data[]|select(.budget_id=="'"$BG_B"'")][0]')
chknum "BLOCK 槽无占用（被拦的单没写占用）已用=0" "$(printf '%s' "$ROW_B" | jq -r '.used')" "0"

say "  · FIX-2：已被取代 / 未生效的版本不出现在执行报表（只返回 ACTIVE）"
check "报表不含旧版 SUPERSEDED（BG_A v1）" \
  "$(printf '%s' "$BODY" | jq -r '[.data[]|select(.budget_id=="'"$BG_A"'")]|length')" "0"

summary
