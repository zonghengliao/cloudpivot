#!/usr/bin/env bash
# ⑨ 收尾核对：折旧重跑幂等（不重复出凭证）+ 卡片详情 API 口径
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
A=$(cat "$EV/raw/asset.id")
B=$(cat "$EV/raw/assetb.id")

echo "===== ⑨-1 重复计提 2026-01（应 EXISTS，不新增批次/凭证）====="
api POST "/business/erp/depreciation/run" "$ADMIN" '{"period_code":"2026-01","remark":"重复计提探测"}' > "$EV/raw/d2_rerun.json"
echo "http=$HTTP $(jq -c '{code,outcome:.data.outcome,total:.data.total_amount,msg:.data.message}' "$EV/raw/d2_rerun.json")"
psqlq "select count(*) as batches_2026_01 from biz_depreciation_batch where period_code = '2026-01'"
psqlq "select count(*) as dep_vouchers from biz_voucher where source_type = 'DEPRECIATION' and source_no = '2026-01'"

echo "===== ⑨-2 资产 A 详情（API 层：状态机可走边 + 累计明细）====="
api GET "/business/erp/asset/$A" "$ADMIN" > "$EV/raw/v1_detail_a.json"
echo "http=$HTTP"
jq -c '{code, asset:{no:.data.asset.asset_no,status:.data.asset.status,orig:.data.asset.original_value,accum:.data.asset.accumulated_depreciation,net:.data.asset.net_value}, months_left:.data.months_left, amount_left:.data.amount_left, next_statuses:.data.next_statuses, changes:(.data.changes|length), depreciations:(.data.depreciations|length)}' "$EV/raw/v1_detail_a.json"

echo "===== ⑨-3 折旧批次详情（合计 = 明细之和）====="
BATCH=$(psqlq "select id from biz_depreciation_batch where period_code = '2026-03'" | sed -n 3p | tr -d ' ')
echo "batch 2026-03 id=$BATCH"
api GET "/business/erp/depreciation/$BATCH" "$ADMIN" > "$EV/raw/v2_batch.json"
echo "http=$HTTP"
jq -c '{code, batch:.data.batch.batch_no, total:.data.batch.total_amount, count:.data.batch.asset_count, detail_sum:([.data.details[].amount]|add), lines:[.data.details[]|{asset_no,amount,is_last_month}]}' "$EV/raw/v2_batch.json"
