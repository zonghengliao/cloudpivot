#!/usr/bin/env bash
# ⑦ 资产停用 / 启用（ST02）：状态机 + 留痕 + 联动通知 + 「IDLE 照提折旧」
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
A=$(cat "$EV/raw/asset.id")

echo "===== ⑦-1 停用（IN_USE → IDLE）====="
api POST "/business/erp/asset/$A/status" "$ADMIN" '{"target_status":"IDLE","change_date":"2026-05-01","reason":"演练停用"}' > "$EV/raw/s1_idle.json"
echo "http=$HTTP $(jq -c '{code,msg,status:.data.asset.status,next:.data.next_statuses}' "$EV/raw/s1_idle.json")"

echo "===== ⑦-2 停用后落库核对 ====="
psqlq "select asset_no, status, accumulated_depreciation, net_value from biz_fixed_asset order by asset_no"
psqlq "select change_type, change_date, from_status, to_status, reason, operator_name, id from biz_fixed_asset_change order by created_at"
psqlq "select entity_type, biz_no, from_status, to_status, reason from biz_status_log where entity_type like '%FIXED%' or biz_no like 'FA-%' order by created_at"
psqlq "select notice_no, biz_type, title, content, status, idempotency_key from sys_notice where biz_type = 'FIXED_ASSET' order by created_at"

echo "===== ⑦-3 停用期间计提 2026-05（设计结论：IDLE 照提折旧）====="
api POST "/business/erp/depreciation/run" "$ADMIN" '{"period_code":"2026-05","remark":"P0-04 停用期间照提"}' > "$EV/raw/s2_dep_idle.json"
echo "http=$HTTP $(jq -c '{outcome:.data.outcome,total:.data.total_amount,count:.data.asset_count}' "$EV/raw/s2_dep_idle.json")"
psqlq "select d.period_code, d.asset_no, d.amount, d.accumulated_after, d.net_value_after from biz_depreciation d where d.period_code = '2026-05'"
psqlq "select asset_no, status, accumulated_depreciation, net_value, depreciation_months_done, last_depreciation_period from biz_fixed_asset order by asset_no"

echo "===== ⑦-4 重复停用（同态）应 409 且不重复通知 ====="
api POST "/business/erp/asset/$A/status" "$ADMIN" '{"target_status":"IDLE","reason":"重复停用"}' > "$EV/raw/s3_dup.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/s3_dup.json")"

echo "===== ⑦-5 启用（IDLE → IN_USE）====="
api POST "/business/erp/asset/$A/status" "$ADMIN" '{"target_status":"IN_USE","change_date":"2026-06-01","reason":"演练启用"}' > "$EV/raw/s4_enable.json"
echo "http=$HTTP $(jq -c '{code,msg,status:.data.asset.status}' "$EV/raw/s4_enable.json")"

echo "===== ⑦-6 启用后：留痕 + 通知（应有 IDLE / IN_USE 两条）====="
psqlq "select change_type, change_date, from_status, to_status, reason from biz_fixed_asset_change order by created_at"
psqlq "select notice_no, biz_type, title, content, idempotency_key from sys_notice where biz_type = 'FIXED_ASSET' order by created_at"
