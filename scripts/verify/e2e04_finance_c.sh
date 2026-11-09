#!/usr/bin/env bash
# E2E-04 财务域演练 · C 段：月结 12 期 → ⑪ 年结 → 反年结 → 再年结 + 全量勾稽 + FIX-6 回归
set -uo pipefail
cd "$(dirname "$0")"
. ./e2e04_finance_common.sh
YEAR=2022

say "=============================================================="
say "C 段 step10：月结（2022 年 12 期逐月封账 + 结账预检）"
say "=============================================================="
api c10_check0 GET "/business/erp/year-end/check?year=$YEAR" "$T_ADMIN"
check "年结预检 http" "$HTTP" "200"
check "未月结时不能年结" "$(data '.data.can_close')" "false"
check "阻塞项 = 期间未结账" "$(data '.data.blockers[0].kind')" "PERIOD_NOT_CLOSED"
check "阻塞项含 12 个未结期间" "$(data '.data.blockers[0].periods|length')" "12"

for m in 01 02 03 04 05 06 07 08 09 10 11 12; do
  PID=$(period_id "2022-$m")
  api "c10_cc$m" GET "/business/erp/period/$PID/close-check" "$T_ADMIN"
  if [ "$(data '.data.can_close')" != "true" ]; then
    bad "2022-$m 结账预检不过：$(data '.data.blockers|map(.message)|join("；")' | head -c 200)"
  fi
  api "c10_close$m" POST "/business/erp/period/$PID/close" "$T_ADMIN" '{"reason":"E2E-04 演练：2022 逐月封账"}'
  if [ "$HTTP" = "200" ] && [ "$CODE" = "0" ]; then
    ok "2022-$m 封账（$(data '.data.status')）"
  else
    bad "2022-$m 封账失败 http=$HTTP $(show)"
  fi
done
check "DB 2022 全年 12 期全 CLOSED" "$(PSQLQ -c "select count(*) from biz_accounting_period where period_code like '2022-%' and status='CLOSED';")" "12"

P06=$(period_id 2022-06); P12=$(period_id 2022-12)
api c10_reopen_bad POST "/business/erp/period/$P06/reopen" "$T_ADMIN" '{"reason":"E2E-04 演练：跳月反结账应当被拦"}'
check "非末月反结账 → 409（必须从最新封的月份往回撤）" "$HTTP" "409"
api c10_close_bad POST "/business/erp/period/$P06/close" "$T_ADMIN" '{"reason":"E2E-04 演练：重复结账"}'
check "重复结账 → 409" "$HTTP" "409"
api c10_vch_closed POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"2022-06-10\",\"summary\":\"E2E-04 演练：往已结账期间记账\",\"entries\":[
  {\"account_id\":\"$ACC_1001\",\"debit\":\"1\"},{\"account_id\":\"$ACC_1002\",\"credit\":\"1\"}]}"
check "已结账期间记账 → 409" "$HTTP" "409"
check "已结账期间未落凭证" "$(PSQLQ -c "select count(*) from biz_voucher where summary like '%往已结账期间记账%';")" "0"
api c10_cc_closed GET "/business/erp/period/$P06/close-check" "$T_ADMIN"
check "已结账期间预检 can_close=false" "$(data '.data.can_close')" "false"

say
say "=============================================================="
say "C 段 step11：⑪ 年度结转（年结）"
say "=============================================================="
api c11_check GET "/business/erp/year-end/check?year=$YEAR" "$T_ADMIN"
check "年结预检 http" "$HTTP" "200"
check "12 期封账后 can_close=true" "$(data '.data.can_close')" "true"
check "阻塞项 0" "$(data '.data.blockers|length')" "0"
check "年结前状态未结转" "$(data '.data.state.closed')" "false"

api c11_close POST /business/erp/year-end/close "$T_ADMIN" '{"year":2022,"reason":"E2E-04 演练：2022 年度结转"}'
check "年结 http" "$HTTP" "200"
check "净利润（本年利润）" "$(money '.data.net_profit')" "240000.00"
check "结转凭证书行数（8 行）" "$(data '.data.entry_count')" "8"
check "封账期间数 12" "$(data '.data.periods_closed')" "12"
check "下年期初已生成" "$(data '.data.opening_balance_created')" "true"
check "下年期初行数 > 0" "$([ "$(data '.data.opening_rows')" -gt 0 ] && echo yes || echo no)" "yes"
check "下一年期间齐备" "$(data '.data.next_year_periods_ready')" "true"
CF_NO=$(data '.data.carry_forward_voucher_no')
save CF_NO "$CF_NO"
OPG_ROWS=$(PSQLQ -c "select count(*) from biz_opening_balance where period_code='2023-01';")
save OPG_ROWS "$OPG_ROWS"
say "  · 结转凭证号 $CF_NO，2023-01 期初 $OPG_ROWS 行"

CF_ID=$(PSQLQ -c "select id from biz_voucher where voucher_no='$CF_NO';")
save CF_ID "$CF_ID"
check "结转凭证类型 YEAR_END" "$(PSQLQ -c "select voucher_type||'/'||period from biz_voucher where id='$CF_ID';")" "YEAR_END/2022-12"
check "结转凭证借贷平衡" "$(PSQLQ -c "select (total_debit=total_credit)::text from biz_voucher where id='$CF_ID';")" "true"
check "结转凭证分录 8 行" "$(PSQLQ -c "select count(*) from biz_voucher_entry where voucher_id='$CF_ID';")" "8"
check "3103 在结转凭证内净额 = 0（两腿相抵）" "$(PSQLQ -c "select coalesce(sum(debit_amount-credit_amount),0)::numeric(20,2)::text from biz_voucher_entry where voucher_id='$CF_ID' and account_code='3103';")" "0.00"
check "3104 贷方 = 净利润 240000" "$(PSQLQ -c "select coalesce(sum(credit_amount-debit_amount),0)::numeric(20,2)::text from biz_voucher_entry where voucher_id='$CF_ID' and account_code='3104';")" "240000.00"
check "5001 收入结平（借 1200000）" "$(PSQLQ -c "select coalesce(sum(debit_amount),0)::numeric(20,2)::text from biz_voucher_entry where voucher_id='$CF_ID' and account_code='5001';")" "1200000.00"

# 年结后：损益类余额归零
PL_LEFT=$(PSQLQ -c "WITH n AS (
  SELECT a.account_code, coalesce(sum(e.debit_amount-e.credit_amount),0) net
    FROM biz_account a LEFT JOIN biz_voucher_entry e ON e.account_code=a.account_code
    LEFT JOIN biz_voucher v ON v.id=e.voucher_id AND v.status='POSTED' AND v.deleted_at IS NULL
   WHERE a.deleted_at IS NULL AND a.account_category='PROFIT_LOSS' AND a.is_leaf=1
   GROUP BY 1)
  SELECT count(*) FROM n WHERE abs(net) > 0.004;")
check "年结后损益类科目余额全为 0（差异行数）" "$PL_LEFT" "0"

# 次年年初余额 = 上年期末
DIFF_OPEN=$(PSQLQ -c "WITH closing AS (
    SELECT e.account_code, sum(e.debit_amount-e.credit_amount) net
      FROM biz_voucher_entry e JOIN biz_voucher v ON v.id=e.voucher_id
     WHERE v.deleted_at IS NULL AND v.status='POSTED' AND v.period BETWEEN '2022-01' AND '2022-12'
     GROUP BY 1),
  op AS (SELECT account_code, debit_opening-credit_opening net FROM biz_opening_balance WHERE period_code='2023-01')
  SELECT count(*) FROM closing c FULL JOIN op o USING (account_code)
   WHERE COALESCE(c.net,0) <> COALESCE(o.net,0);")
check "2023-01 期初 = 2022 期末（差异行数）" "$DIFF_OPEN" "0"
check "2023-01 期初行来源全为 YEAR_END" "$(PSQLQ -c "select count(*) from biz_opening_balance where period_code='2023-01' and source<>'YEAR_END';")" "0"
check "年度状态留痕（ACCOUNTING_YEAR=2022 → CLOSED）" "$(PSQLQ -c "select to_status from biz_status_log where entity_type='ACCOUNTING_YEAR' and entity_no='2022' order by created_at desc limit 1;")" "CLOSED"

# 年结后不能记账（12 月已封）
api c11_vch_after POST /business/erp/voucher "$T_ADMIN" "{\"voucher_date\":\"2022-12-31\",\"summary\":\"E2E-04 演练：年结后补记 12 月\",\"entries\":[
  {\"account_id\":\"$ACC_1001\",\"debit\":\"1\"},{\"account_id\":\"$ACC_1002\",\"credit\":\"1\"}]}"
check "年结后往 2022-12 记账 → 409" "$HTTP" "409"
check "年结后未落补记凭证" "$(PSQLQ -c "select count(*) from biz_voucher where summary like '%年结后补记%';")" "0"

# CLS-01（7591405）：已年结的年度不许单独反结账
P11=$(period_id 2022-11)
api c11_reopen_blocked POST "/business/erp/period/$P11/reopen" "$T_ADMIN" '{"reason":"E2E-04 演练：已年结年度单独反结账应当被拦"}'
check "已年结年度单独反结账 → 409" "$HTTP" "409"
printf '%s' "$BODY" | jq -r '.msg' | grep -q "年度" && ok "拦截图点名「年度」" || bad "拦截文案未提年度：$(data '.msg' | head -c 120)"
check "拦截后 2022-11 仍 CLOSED" "$(PSQLQ -c "select status from biz_accounting_period where id='$P11';")" "CLOSED"

api c11_check_closed GET "/business/erp/year-end/check?year=$YEAR" "$T_ADMIN"
check "已年结后再预检 can_close=false" "$(data '.data.can_close')" "false"
check "阻塞项含 ALREADY_CLOSED" "$(data '.data.blockers[0].kind')" "ALREADY_CLOSED"
api c11_close_again POST /business/erp/year-end/close "$T_ADMIN" '{"year":2022}'
check "重复年结 → 409" "$HTTP" "409"

say
say "=============================================================="
say "C 段 step12：反年结 → 重新结 12 月 → 再年结（闭环）"
say "=============================================================="
api c12_reopen POST /business/erp/year-end/reopen "$T_ADMIN" '{"year":2022,"reason":"E2E-04 演练：反年结验证（口径核对）"}'
check "反年结 http" "$HTTP" "200"
check "12 月已解封" "$(data '.data.december_reopened')" "true"
check "结转凭证已删" "$(data '.data.deleted_voucher_no')" "$CF_NO"
check "下年期初行已删" "$([ "$(data '.data.deleted_opening_rows')" -gt 0 ] && echo yes || echo no)" "yes"
check "DB 2022-12 已 OPEN" "$(PSQLQ -c "select status from biz_accounting_period where period_code='2022-12';")" "OPEN"
check "DB 结转凭证已软删" "$(PSQLQ -c "select count(*) from biz_voucher where id='$CF_ID' and deleted_at is null;")" "0"
check "DB 2023-01 期初行已清" "$(PSQLQ -c "select count(*) from biz_opening_balance where period_code='2023-01';")" "0"
api c12_check_after GET "/business/erp/year-end/check?year=$YEAR" "$T_ADMIN"
check "反年结后预检 can_close=false" "$(data '.data.can_close')" "false"
check "阻塞项 = PERIOD_NOT_CLOSED（12 月）" "$(data '.data.blockers[0].kind')" "PERIOD_NOT_CLOSED"
check "点名 2022-12" "$(data '.data.blockers[0].periods|join(",")')" "2022-12"

P12=$(period_id 2022-12)
api c12_close_dec POST "/business/erp/period/$P12/close" "$T_ADMIN" '{"reason":"E2E-04 演练：反年结后重新封 12 月"}'
check "重新封 12 月 http" "$HTTP" "200"
api c12_close_year POST /business/erp/year-end/close "$T_ADMIN" '{"year":2022,"reason":"E2E-04 演练：重新年结"}'
check "再年结 http" "$HTTP" "200"
check "再年结净利润" "$(money '.data.net_profit')" "240000.00"
check "再年结后 2023-01 期初行数不变" "$(PSQLQ -c "select count(*) from biz_opening_balance where period_code='2023-01';")" "$OPG_ROWS"
NEW_CF=$(data '.data.carry_forward_voucher_no'); save NEW_CF "$NEW_CF"
checkne "结转凭证号已换新" "$NEW_CF" "$CF_NO"
check "2023-01 期初 = 2022 期末（再年结后仍成立）" "$(PSQLQ -c "WITH closing AS (
    SELECT e.account_code, sum(e.debit_amount-e.credit_amount) net
      FROM biz_voucher_entry e JOIN biz_voucher v ON v.id=e.voucher_id
     WHERE v.deleted_at IS NULL AND v.status='POSTED' AND v.period BETWEEN '2022-01' AND '2022-12'
     GROUP BY 1),
  op AS (SELECT account_code, debit_opening-credit_opening net FROM biz_opening_balance WHERE period_code='2023-01')
  SELECT count(*) FROM closing c FULL JOIN op o USING (account_code)
   WHERE COALESCE(c.net,0) <> COALESCE(o.net,0);")" "0"
check "年度状态最新一行 CLOSED" "$(PSQLQ -c "select to_status from biz_status_log where entity_type='ACCOUNTING_YEAR' and entity_no='2022' order by created_at desc limit 1;")" "CLOSED"

say
say "=============================================================="
say "C 段 step13：年结后的三大报表 + 全量勾稽"
say "=============================================================="
line_amt() { printf '%s' "$BODY" | jq -r "((.data.$1[]? | select(.item_code==\"$2\") | .amount) // 0) | tonumber | (.*100|round/100)" 2>/dev/null; }
api c13_bs GET "/business/erp/report/balance-sheet?period=2022-12" "$T_ADMIN"
check "年结后资产负债表平衡" "$(data '.data.check.balanced')" "true"
check "年结后勾稽差额" "$(money '.data.check.diff')" "0.00"
check "年结后未分配利润仍 740000" "$(line_amt equity UNDISTRIBUTED_PROFIT)" "740000"
check "年结后应交税费仍按 0 列示" "$(line_amt liabilities TAXES_PAYABLE)" "0"
check "年结后重分类仍生效" "$(data '.data.reclass|length')" "1"
check "年结后资产总计" "$(line_amt assets TOTAL_ASSETS)" "2118600"
api c13_is GET "/business/erp/report/income-statement?period=2022-12" "$T_ADMIN"
is_amt() { printf '%s' "$BODY" | jq -r "((.data.items[]? | select(.item_code==\"$1\") | .$2) // 0) | tonumber | (.*100|round/100)"; }
check "年结后利润表净利润（结转凭证不进本期发生额）" "$(is_amt NET_PROFIT ytd)" "240000"
check "年结后利润表勾稽" "$(data '.data.check.balanced')" "true"
check "年结后净利润 = 权益变动" "$(money '.data.check.net_profit_ytd')" "$(money '.data.check.equity_change')"
api c13_cf GET "/business/erp/report/cash-flow?period=2022-12" "$T_ADMIN"
check "年结后现金流量表勾稽" "$(data '.data.check.balanced')" "true"
check "年结后期末现金" "$(money '.data.check.closing_balance')" "1616000.00"
check "年结后现金流量表 ↔ 货币资金差额" "$(money '.data.check.diff')" "0.00"
# 2023-01 首月：期初 = 上年期末（报表口径，不靠 OPENING 凭证双计）
api c13_bs2301 GET "/business/erp/report/balance-sheet?period=2023-01" "$T_ADMIN"
check "2023-01 资产负债表平衡" "$(data '.data.check.balanced')" "true"
check "2023-01 货币资金 = 上年期末" "$(line_amt assets MONETARY_FUNDS)" "1616000"
check "2023-01 未分配利润 = 740000（不双计）" "$(line_amt equity UNDISTRIBUTED_PROFIT)" "740000"

say
say "=============================================================="
say "C 段 step14：FIX-6 回归（留抵重分类 / tax_rate 标度）"
say "=============================================================="
# tax_rate 标度：库内不得再有 (0,1] 未标注的税率
check "biz_invoice 无 (0,1] 未标注税率" "$(PSQLQ -c "select count(*) from biz_invoice where tax_rate > 0 and tax_rate <= 1 and tax_rate_mode is null;")" "0"
check "biz_material 无 (0,1] 未标注税率" "$(PSQLQ -c "select count(*) from biz_material where tax_rate > 0 and tax_rate <= 1 and tax_rate_mode is null;")" "0"
# 新建单位+物料，验证 0.13 → 13（DECIMAL 过渡期）+ 上界 100
api c14_unit POST /business/erp/unit "$T_ADMIN" '{"unit_code":"E2E04-PCS","unit_name":"个(E2E04)","is_base":true,"status":1,"precision":0}'
UNIT_ID=$(data '.data.id')
[ -z "$UNIT_ID" ] && UNIT_ID=$(PSQLQ -c "select id from biz_unit where unit_code='E2E04-PCS' and deleted_at is null;")
save UNIT_ID "$UNIT_ID"
api c14_mat POST /business/erp/material "$T_ADMIN" "{\"material_code\":\"E2E04-MAT\",\"material_name\":\"E2E-04 演练物料\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"RAW\",\"tax_rate\":0.13,\"status\":1,\"remark\":\"E2E-04 演练物料（税率标度回归）\"}"
MAT_ID=$(data '.data.id')
[ -z "$MAT_ID" ] && MAT_ID=$(PSQLQ -c "select id from biz_material where material_code='E2E04-MAT' and deleted_at is null;")
save MAT_ID "$MAT_ID"
check "物料 tax_rate 0.13 → 归一 13" "$(PSQLQ -c "select tax_rate::text from biz_material where id='$MAT_ID';")" "13.0000"
check "物料 tax_rate_mode=DECIMAL（过渡期标注）" "$(PSQLQ -c "select coalesce(tax_rate_mode,'') from biz_material where id='$MAT_ID';")" "DECIMAL"
api c14_mat_bad PUT "/business/erp/material/$MAT_ID" "$T_ADMIN" "{\"material_code\":\"E2E04-MAT\",\"material_name\":\"E2E-04 演练物料\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"RAW\",\"tax_rate\":100.5,\"status\":1}"
check "物料税率 100.5 → 400" "$HTTP" "400"
api c14_mat_ok PUT "/business/erp/material/$MAT_ID" "$T_ADMIN" "{\"material_code\":\"E2E04-MAT\",\"material_name\":\"E2E-04 演练物料\",\"unit_id\":\"$UNIT_ID\",\"material_type\":\"RAW\",\"tax_rate\":13,\"status\":1,\"remark\":\"E2E-04 演练物料（税率标度回归）\"}"
check "物料税率 13（百分数）→ 200" "$HTTP" "200"
check "回写 13 + PERCENT" "$(PSQLQ -c "select tax_rate::text||'/'||coalesce(tax_rate_mode,'') from biz_material where id='$MAT_ID';")" "13.0000/PERCENT"

say
say "=============================================================="
say "C 段 step15：全库不变量（收尾基线）"
say "=============================================================="
check "全库凭证借贷平衡（不平张数）" "$(vch_unbalanced)" "0"
check "全库 POSTED 凭证均有分录" "$(PSQLQ -c "select count(*) from biz_voucher v where v.deleted_at is null and v.status='POSTED' and not exists (select 1 from biz_voucher_entry e where e.voucher_id=v.id);")" "0"
check "凭证期间必有期间行" "$(PSQLQ -c "select count(*) from biz_voucher v where v.deleted_at is null and not exists (select 1 from biz_accounting_period p where p.period_code=v.period);")" "0"
check "2022 全年凭证借贷合计相等" "$(PSQLQ -c "select abs(coalesce(sum(case when v.status='POSTED' then (select sum(debit_amount) from biz_voucher_entry e where e.voucher_id=v.id) else 0 end),0) - coalesce(sum(case when v.status='POSTED' then (select sum(credit_amount) from biz_voucher_entry e where e.voucher_id=v.id) else 0 end),0))::numeric(20,2)::text from biz_voucher v where v.deleted_at is null and v.period between '2022-01' and '2022-12';")" "0.00"
check "2022 无停在 DRAFT/AUDITED 的凭证" "$(PSQLQ -c "select count(*) from biz_voucher where deleted_at is null and period like '2022-%' and status<>'POSTED';")" "0"

summary
