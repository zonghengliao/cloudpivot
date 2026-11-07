#!/usr/bin/env bash
# ⑥ 连续计提：2026-02 / 2026-03（满 3 个月）+ 2026-04（B 提足即停）
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")

run() { # $1=period
  api POST "/business/erp/depreciation/run" "$ADMIN" "{\"period_code\":\"$1\",\"remark\":\"P0-04 演练计提 $1\"}" > "$EV/raw/dep_run_$1.json"
  echo "[$1] http=$HTTP $(jq -c '{outcome:.data.outcome,total:.data.total_amount,count:.data.asset_count,msg:.data.message}' "$EV/raw/dep_run_$1.json")"
}

echo "===== ⑥-1 2026-02 ====="; run 2026-02
echo "===== ⑥-2 2026-03 ====="; run 2026-03
echo "===== ⑥-3 2026-04（资产 B 已提足，应只剩 A 一台）====="; run 2026-04

echo "===== ⑥-4 逐月明细（月份 / 本期折旧 / 累计 / 净值）====="
psqlq "select d.asset_no, d.period_code, d.amount as this_month, d.accumulated_after, d.net_value_after, d.is_last_month from biz_depreciation d order by d.asset_no, d.period_code"
psqlq "select d.period_code, count(*) as assets, sum(d.amount) as total from biz_depreciation d group by d.period_code order by d.period_code"

echo "===== ⑥-5 卡片当前累计/净值 + 防超提断言 ====="
psqlq "select asset_no, original_value, residual_value, depreciable_base, accumulated_depreciation, net_value, depreciation_months_done, (accumulated_depreciation <= depreciable_base) as not_over, (net_value = original_value - accumulated_depreciation) as net_identity from biz_fixed_asset order by asset_no"

echo "===== ⑥-6 批次表（每期一张凭证）====="
psqlq "select batch_no, period_code, status, asset_count, total_amount, voucher_id from biz_depreciation_batch order by period_code"
