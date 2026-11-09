#!/usr/bin/env bash
# E2E-04 财务域演练 · B 段：⑤ 总账 ⑥ 资产负债表 ⑦ 利润表 ⑧ 现金流量表 ⑨ 应付账龄 ⑩ 报表项维护
set -uo pipefail
cd "$(dirname "$0")"
. ./e2e04_finance_common.sh

line_amt() { # line_amt <assets|liabilities|equity> <item_code>
  printf '%s' "$BODY" | jq -r "((.data.$1[]? | select(.item_code==\"$2\") | .amount) // 0) | tonumber | (.*100|round/100)" 2>/dev/null
}

say "=============================================================="
say "B 段 step5：⑤ 总账（明细账 / 余额表 / 辅助账）"
say "=============================================================="
api b5_ledger GET "/business/erp/ledger/page?account_code=1002&period_from=2022-01-01&period_to=2022-12-31&page=1&page_size=200" "$T_ADMIN"
check "明细账 http" "$HTTP" "200"
LED_N=$(data '.data.list|length')
check "1002 明细账行数（期初1 + 收12 + 付12 + 划转1）" "$LED_N" "26"
LAST_CLOSE=$(printf '%s' "$BODY" | jq -r '[.data.list[].running_balance|tonumber] | last // 0 | (.*100|round/100)')
check "明细账末行余额 = 科目净额" "$LAST_CLOSE" "$(acc_net 1002 2022-01-01 2022-12-31 | awk '{printf "%.2f", $1+0}')"
check "明细账末行余额 = 1614500（1616000 − 划转 1500）" "$LAST_CLOSE" "1614500"
api b5_ledger_nokey GET "/business/erp/ledger/page?period_from=2022-01-01" "$T_ADMIN"
check "明细账缺科目 → 400" "$HTTP" "400"

api b5_tb GET "/business/erp/ledger/trial-balance?period=2022-12" "$T_ADMIN"
check "余额表 http" "$HTTP" "200"
check "余额表期初借贷相等" "$(money '.data.total_opening_debit')" "$(money '.data.total_opening_credit')"
check "余额表本期借贷相等" "$(money '.data.total_period_debit')" "$(money '.data.total_period_credit')"
check "余额表本期借方 = 12 月发生的借方合计" "$(money '.data.total_period_debit')" "$(month_debit 2022-12-01 2022-12-31)"
TB_ROW=$(printf '%s' "$BODY" | jq -r '[.data.rows[]|select(.account_code=="1405")]|length')
check "余额表含存货科目行" "$TB_ROW" "1"
api b5_tb_bad GET "/business/erp/ledger/trial-balance" "$T_ADMIN"
check "余额表缺 period → 400" "$HTTP" "400"

# 辅助账：2022 年凭证全是手工凭证（手工分录无辅助核算字段）→ 未挂维度的发生额要被点名
api b5_aux GET "/business/erp/ledger/aux-balance?aux_type=DEPT&period_from=2022-01&period_to=2022-12" "$T_ADMIN"
check "辅助账 2022 http" "$HTTP" "200"
check "辅助账 2022 无部门维度行（手工凭证不带辅助核算）" "$(data '.data.rows|length')" "0"
checkne "辅助账 2022 未挂维度笔数 > 0（不静默丢数）" "$([ "$(data '.data.unassigned.entry_count')" -gt 0 ] && echo yes || echo no)" "no"
api b5_aux2 GET "/business/erp/ledger/aux-balance?aux_type=MATERIAL&period_from=2026-11&period_to=2026-11" "$T_ADMIN"
check "辅助账物料维度 2026-11 http" "$HTTP" "200"
check "辅助账物料维度行带 aux_name（无行则接受 empty）" "$(printf '%s' "$BODY" | jq -r 'if (.data.rows|length)>0 then (.data.rows[0]|has("aux_name")|tostring) else "empty" end')" "$(printf '%s' "$BODY" | jq -r 'if (.data.rows|length)>0 then "true" else "empty" end')"

say
say "=============================================================="
say "B 段 step6：⑥ 资产负债表（勾稽 + FIX-6 留抵重分类）"
say "=============================================================="
api b6_bs GET "/business/erp/report/balance-sheet?period=2022-12" "$T_ADMIN"
check "资产负债表 http" "$HTTP" "200"
check "资产 = 负债 + 权益" "$(data '.data.check.balanced')" "true"
check "勾稽差额" "$(money '.data.check.diff')" "0.00"
check "货币资金" "$(line_amt assets MONETARY_FUNDS)" "1616000"
check "存货" "$(line_amt assets INVENTORY)" "500000"
check "应收账款（本年收清为 0）" "$(line_amt assets ACCOUNTS_RECEIVABLE)" "0"
check "应付账款" "$(line_amt liabilities ACCOUNTS_PAYABLE)" "1378600"
check "应交税费（留抵按 0 列示，不许是负数）" "$(line_amt liabilities TAXES_PAYABLE)" "0"
check "其他流动资产（留抵重分类进来的 2600）" "$(line_amt assets OTHER_CURRENT_ASSETS)" "2600"
check "未分配利润（期初 500000 + 本年 240000）" "$(line_amt equity UNDISTRIBUTED_PROFIT)" "740000"
check "资产总计" "$(line_amt assets TOTAL_ASSETS)" "2118600"
check "负债和所有者权益总计" "$(line_amt equity TOTAL_LIABILITIES_AND_EQUITY)" "2118600"
BS_PREV=$(data '.data.previous_period')
check "12 月报表带上期金额列（上期=2022-11）" "$BS_PREV" "2022-11"
# FIX-6 回归：留抵税额重分类
RC_N=$(data '.data.reclass|length')
check "重分类条数" "$RC_N" "1"
check "重分类 from → to" "$(data '.data.reclass[0].from_item + "→" + .data.reclass[0].to_item')" "TAXES_PAYABLE→OTHER_CURRENT_ASSETS"
check "重分类金额（销项 156000 − 进项 158600）" "$(money '.data.reclass[0].amount')" "2600.00"
check "重分类来源科目" "$(data '.data.reclass[0].accounts|sort|join(",")')" "2221,2221.01,2221.02"
printf '%s' "$BODY" | jq -r '.data.notes[]?' | grep -q "留抵" && ok "附注点名「留抵税额」" || bad "附注未提留抵：$(data '.data.notes|join(" / ")')"
# 扣减项（累计折旧）为负号口径
check "累计折旧行金额（无固定资产 → 0）" "$(line_amt assets ACCUM_DEPRECIATION)" "0"
api b6_bs11 GET "/business/erp/report/balance-sheet?period=2022-11" "$T_ADMIN"
check "11 月资产负债表仍平衡" "$(data '.data.check.balanced')" "true"
api b6_bs_bad GET "/business/erp/report/balance-sheet?period=2022-13" "$T_ADMIN"
check "非法期间 → 400" "$HTTP" "400"

say
say "=============================================================="
say "B 段 step7：⑦ 利润表（净利润 = 权益变动）"
say "=============================================================="
api b7_is GET "/business/erp/report/income-statement?period=2022-12" "$T_ADMIN"
check "利润表 http" "$HTTP" "200"
is_amt() { printf '%s' "$BODY" | jq -r "((.data.items[]? | select(.item_code==\"$1\") | .$2) // 0) | tonumber | (.*100|round/100)"; }
check "营业收入（本期 12 月）" "$(is_amt REVENUE current)" "100000"
check "营业收入（本年累计）" "$(is_amt REVENUE ytd)" "1200000"
check "营业成本（本年累计）" "$(is_amt COST_OF_SALES ytd)" "720000"
check "管理费用（本年累计）" "$(is_amt ADMIN_EXPENSE ytd)" "240000"
check "净利润（本年累计）" "$(is_amt NET_PROFIT ytd)" "240000"
check "勾稽：净利润 = 权益变动" "$(data '.data.check.balanced')" "true"
check "净利润" "$(money '.data.check.net_profit_ytd')" "240000.00"
check "权益变动（未分配利润本年变动）" "$(money '.data.check.equity_change')" "240000.00"
check "勾稽差额" "$(money '.data.check.diff')" "0.00"
check "未归类损益（应为 0）" "$(money '.data.check.unmapped_profit_loss_ytd')" "0.00"
check "年初未分配利润" "$(money '.data.check.undistributed_profit_year_start')" "500000.00"
check "期末未分配利润" "$(money '.data.check.undistributed_profit_end')" "740000.00"

say
say "=============================================================="
say "B 段 step8：⑧ 现金流量表（期末现金 = 资产负债表货币资金）"
say "=============================================================="
api b8_cf GET "/business/erp/report/cash-flow?period=2022-12" "$T_ADMIN"
check "现金流量表 http" "$HTTP" "200"
check "现金流量表勾稽 balanced" "$(data '.data.check.balanced')" "true"
check "期末现金（本表）" "$(money '.data.check.closing_balance')" "1616000.00"
check "期末现金（总账口径）" "$(money '.data.check.closing_by_ledger')" "1616000.00"
check "现金流量表 ↔ 资产负债表货币资金差额" "$(money '.data.check.diff')" "0.00"
check "本年累计净增加额（销售收现 1356000 − 费用付现 240000）" "$(money '.data.net_increase.ytd')" "1116000.00"
check "期初现金（本年累计口径）" "$(money '.data.opening_balance.ytd')" "500000.00"
check "未分类现金笔数（应为 0）" "$(data '.data.check.unclassified_count')" "0"
check "经营活动净额（本年累计）" "$(money '.data.operating.net.ytd')" "1116000.00"

say
say "=============================================================="
say "B 段 step9：⑨ 应付账龄分析（2026-11 真实应付数据）"
say "=============================================================="
api b9_aging GET "/business/erp/report/payable-aging?period=2026-11" "$T_ADMIN"
check "应付账龄 http" "$HTTP" "200"
check "基准日来源 PERIOD" "$(data '.data.as_of_source')" "PERIOD"
AG_DOC=$(data '.data.doc_count')
AG_UNPAID=$(money '.data.total_unpaid')
AG_SUM=$(printf '%s' "$BODY" | jq -r '[.data.summary[].unpaid|tonumber]|add // 0' | awk '{printf "%.2f", $1+0}')
check "未付合计 = 各档之和" "$AG_UNPAID" "$AG_SUM"
DB_AG=$(PSQLQ -c "select coalesce(sum(unpaid_amount),0)::numeric(20,2)::text||'|'||count(*) from biz_payable
                   where deleted_at is null and status in ('OPEN','PARTIAL_PAID','PENDING_CANCEL')
                     and unpaid_amount>0 and period_code<='2026-11';")
check "未付合计/单据数 与 biz_payable 一致" "$AG_UNPAID|$AG_DOC" "$DB_AG"
check "未付 = 应付 − 已付" "$(money '.data.total_unpaid')" "$(printf '%s' "$BODY" | jq -r '((.data.total_amount|tonumber)-(.data.total_paid|tonumber))' | awk '{printf "%.2f", $1+0}')"
check "供应商数 = 供应商汇总行数" "$(data '.data.supplier_count')" "$(data '.data.suppliers|length')"
check "明细条数不超上限" "$([ "$(data '.data.details|length')" -le 1000 ] && echo ok || echo overflow)" "ok"
api b9_aging_asof GET "/business/erp/report/payable-aging?as_of=2026-11-30" "$T_ADMIN"
check "as_of 直传 http" "$HTTP" "200"
check "as_of 回显" "$(data '.data.as_of')" "2026-11-30"
api b9_aging_empty GET "/business/erp/report/payable-aging?period=2022-12" "$T_ADMIN"
check "无应付年度账龄为空（单据数 0）" "$(data '.data.doc_count')" "0"
check "无应付年度未付合计 0" "$(money '.data.total_unpaid')" "0.00"

say
say "=============================================================="
say "B 段 step10：⑩ 报表项维护（科目 ↔ 报表项映射）"
say "=============================================================="
api b10_list GET "/business/erp/report-item/list" "$T_ADMIN"
check "报表项列表 http" "$HTTP" "200"
ITEM_N=$(data '.data|length')
checkne "报表项总数 > 50" "$([ "${ITEM_N:-0}" -gt 50 ] && echo yes || echo no)" "no"
api b10_bs_list GET "/business/erp/report-item/list?report_code=BALANCE_SHEET" "$T_ADMIN"
check "按报表过滤无越界" "$(data '[.data[]|select(.report_code!="BALANCE_SHEET")]|length')" "0"
api b10_catalog GET "/business/erp/report-item/catalog" "$T_ADMIN"
check "字典 http" "$HTTP" "200"
check "字典含两张报表" "$(data '.data.reports|length')" "2"
check "字典项数 = 列表条数" "$(data '.data.total_items')" "$ITEM_N"
api b10_preview POST /business/erp/report-item/preview "$T_ADMIN" '{"report_code":"BALANCE_SHEET","account_pattern":"1002"}'
check "取数预览命中 1 个科目" "$(data '.data.matched_count')" "1"
check "预览回显科目编码" "$(data '.data.matched_accounts[0].account_code')" "1002"
api b10_preview0 POST /business/erp/report-item/preview "$T_ADMIN" '{"report_code":"BALANCE_SHEET","account_pattern":"9999"}'
check "预览 0 命中带告警" "$([ "$(data '.data.warnings|length')" -gt 0 ] && echo yes || echo no)" "yes"
api b10_preview_bad POST /business/erp/report-item/preview "$T_ADMIN" '{"report_code":"BALANCE_SHEET","formula":"NO_SUCH_ITEM + 1"}'
check "非法公式被识别（400 或 problems 非空）" "$([ "$HTTP" = "400" ] || [ "$(data '.data.problems|length')" -gt 0 ] && echo blocked || echo "http=$HTTP")" "blocked"

api b10_create POST /business/erp/report-item "$T_ADMIN" '{"report_code":"BALANCE_SHEET","item_code":"E2E04_TEST","item_name":"E2E04 演练项","item_level":2,"side":"ASSET","account_pattern":"9999","is_active":false,"sort_order":990,"remark":"E2E-04 演练用（保持停用）"}'
check "新增报表项 http（重跑时 409 幂等）" "$([ "$HTTP" = "200" ] || [ "$HTTP" = "409" ] && echo ok || echo "$HTTP")" "ok"
ITEM_ID=$(data '.data.item.id')
if [ -z "$ITEM_ID" ] || [ "$ITEM_ID" = "null" ]; then
  ITEM_ID=$(data '.data.id')   # 重跑时后端回 409，从列表里取既有行
  api b10_list2 GET "/business/erp/report-item/list?report_code=BALANCE_SHEET" "$T_ADMIN"
  ITEM_ID=$(data '.data[]? | select(.item_code=="E2E04_TEST") | .id')
  say "  · 报表项 E2E04_TEST 已存在（重跑），复用 $ITEM_ID"
fi
save ITEM_ID "$ITEM_ID"
check "新增项落库且默认停用" "$(PSQLQ -c "select case when is_active=0 then 'false' else 'true' end from biz_report_item where id='$ITEM_ID';")" "false"
api b10_update PUT "/business/erp/report-item/$ITEM_ID" "$T_ADMIN" '{"item_code":"E2E04_TEST","report_code":"BALANCE_SHEET","item_name":"E2E04 演练项(改名)","item_level":2,"side":"ASSET","account_pattern":"9999","is_active":false,"sort_order":990,"remark":"E2E-04 演练用（保持停用）"}'
check "编辑报表项 http" "$HTTP" "200"
check "改名生效" "$(data '.data.item.item_name')" "E2E04 演练项(改名)"
api b10_toggle_on PUT "/business/erp/report-item/$ITEM_ID/toggle" "$T_ADMIN" '{"is_active":true}'
check "启用报表项" "$(data '.data.is_active')" "true"
api b10_toggle_off PUT "/business/erp/report-item/$ITEM_ID/toggle" "$T_ADMIN" '{"is_active":false}'
check "停用报表项" "$(data '.data.is_active')" "false"
api b10_bs_ids GET "/business/erp/report-item/list?report_code=BALANCE_SHEET" "$T_ADMIN"
BS_ITEM_N=$(data '.data|length')
IDS=$(data '[.data[].id]')
TOTAL_ITEM_ID=$(data '.data[]? | select(.item_code=="CURRENT_ASSETS_TOTAL") | .id')
api b10_toggle_ref PUT "/business/erp/report-item/$TOTAL_ITEM_ID/toggle" "$T_ADMIN" '{"is_active":false}'
check "被公式引用的项不许停用（409）" "$HTTP" "409"
api b10_sort PUT /business/erp/report-item/sort "$T_ADMIN" "{\"report_code\":\"BALANCE_SHEET\",\"ids\":$IDS}"
check "整表重排（保持原序）http" "$HTTP" "200"
check "重排后条目数不变" "$(data '.data|length')" "$BS_ITEM_N"
api b10_bs_after GET "/business/erp/report/balance-sheet?period=2022-12" "$T_ADMIN"
check "改完报表项后资产负债表仍平衡" "$(data '.data.check.balanced')" "true"

summary
