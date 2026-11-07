#!/usr/bin/env bash
# ③④⑤ 折旧计算核对 + 计提第 1 个月（2026-01）+ 折旧凭证
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
A=$(cat "$EV/raw/asset.id"); B=$(cat "$EV/raw/assetb.id")

echo "===== ②-附 购入凭证（借 1601 / 贷 2202）====="
psqlq "select v.voucher_no, v.source_type, v.source_no, v.voucher_date, v.total_debit, v.total_credit, e.line_no, e.account_code, e.account_name, e.debit_amount, e.credit_amount from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id where v.source_type = 'FIXED_ASSET_ACQ' order by v.voucher_no, e.line_no"

echo "===== ③ 折旧计算手工核对（卡片字段）====="
psqlq "select asset_no, original_value, residual_value, depreciable_base, useful_life_months, monthly_depreciation, round(original_value - residual_value, 4) as hand_base, round((original_value - residual_value)/useful_life_months, 4) as hand_monthly, (monthly_depreciation = round((original_value - residual_value)/useful_life_months, 4)) as monthly_matches from biz_fixed_asset order by asset_no"

echo "===== ④ 计提 2026-01 ====="
api POST "/business/erp/depreciation/run" "$ADMIN" '{"period_code":"2026-01","remark":"P0-04 演练第 1 期"}' > "$EV/raw/d1_run.json"
echo "http=$HTTP"
jq -c '{code,msg,outcome:.data.outcome,total:.data.total_amount,count:.data.asset_count,batch_no:.data.batch.batch_no,status:.data.batch.status}' "$EV/raw/d1_run.json"

echo "===== ④-2 批次 + 明细落库 ====="
psqlq "select b.batch_no, b.period_code, b.status, b.asset_count, b.total_amount, b.voucher_id, b.is_red from biz_depreciation_batch b order by b.period_code"
psqlq "select d.period_code, d.asset_no, d.dept_name, d.expense_account_code, d.amount, d.accumulated_before, d.accumulated_after, d.net_value_after, d.is_last_month, d.status from biz_depreciation d order by d.period_code, d.asset_no"

echo "===== ④-3 卡片累计/净值 ====="
psqlq "select asset_no, accumulated_depreciation, net_value, depreciation_months_done, last_depreciation_period from biz_fixed_asset order by asset_no"

echo "===== ⑤ 折旧凭证（借 5602.01 / 贷 1602）====="
psqlq "select v.voucher_no, v.source_type, v.source_no, v.voucher_date, v.period, v.total_debit, v.total_credit, e.line_no, e.account_code, e.account_name, e.debit_amount, e.credit_amount, e.aux_dept_id from biz_voucher v join biz_voucher_entry e on e.voucher_id = v.id where v.source_type = 'DEPRECIATION' order by v.voucher_no, e.line_no"
