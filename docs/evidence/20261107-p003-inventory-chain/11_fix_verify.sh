#!/usr/bin/env bash
# 修复后复跑：同一漂移场景（汇总桶 + 库位桶并存 + 明细指定库位）必须两侧同桶、逐桶相等
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
LEAD=$(cat "$EV/raw/demo_lead.token")
A=$(cat "$EV/raw/wh_a.id"); B=$(cat "$EV/raw/wh_b.id")
SUP="01a1b5d2-504f-72f7-92b2-00f884285a2c"
UNIT="68725745-ee24-4d9d-8ede-8fd28e6068f0"
PSQL="ssh root@172.18.10.145 \"$PGENV -tAc\""

echo "===== 0 建第二只演练物料 DEMO-INV02 ====="
api POST '/business/erp/material' "$ADMIN" "{
  \"material_code\":\"DEMO-INV02\",\"material_name\":\"库存演练物料B桶验证\",\"unit_id\":\"$UNIT\",
  \"material_type\":\"RAW\",\"costing_method\":\"MOVING_AVG\",\"tax_rate\":0,\"safety_stock\":0,\"status\":1
}" > "$EV/raw/11_material2.json"
echo "http=$HTTP $(jq -c '{code,msg,id:.data.id,code2:.data.material_code}' "$EV/raw/11_material2.json")"
MAT2=$(jq -r '.data.id' "$EV/raw/11_material2.json"); echo "$MAT2" > "$EV/raw/material2.id"

echo "===== 1 期初 100 @ 10（期间 2026-09，落「汇总桶」）====="
api POST '/business/erp/opening-balance/inventory' "$ADMIN" "{
  \"period\":\"2026-09\",
  \"items\":[{\"material_id\":\"$MAT2\",\"warehouse_id\":\"$A\",\"qty\":100,\"unit_cost\":10,\"remark\":\"P0-03 修复验证期初\"}]
}" > "$EV/raw/11_opening2.json"
echo "http=$HTTP $(jq -c '{code,msg,voucher:.data.voucher.voucher_no,debit:.data.voucher.total_debit}' "$EV/raw/11_opening2.json")"

echo "===== 2 直接入库 50 @ 12（落「默认库位桶」）====="
api POST '/business/erp/stock-in' "$ADMIN" "{
  \"is_sourceless\":true,\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$A\",
  \"remark\":\"P0-03 修复验证：入库 50@12\",\"items\":[{\"material_id\":\"$MAT2\",\"qty\":50,\"price_excl\":12,\"tax_rate\":0}]
}" > "$EV/raw/11_stockin2.json"
IN2=$(jq -r '.data.header.id' "$EV/raw/11_stockin2.json")
echo "http=$HTTP id=$IN2 no=$(jq -r '.data.header.in_no' "$EV/raw/11_stockin2.json")"
api POST "/business/erp/stock-in/$IN2/submit-post" "$ADMIN" '{}' > "$EV/raw/11_stockin2_post.json"
echo "post http=$HTTP status=$(jq -r '.data.header.status' "$EV/raw/11_stockin2_post.json")"

LOCBUCKET=$(ssh root@172.18.10.145 "export PGPASSWORD=$PG_PASS; psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X -tAc \"select id from biz_inventory_location where material_id='$MAT2'::uuid and warehouse_id='$A'::uuid and location_id is not null limit 1\"")
echo "库位桶 id=$LOCBUCKET"

echo "===== 3 调拨 40 件（明细指定库位桶；仓里有汇总桶 → 旧代码在这里漂移）====="
api POST '/business/erp/transfer' "$ADMIN" "{
  \"from_warehouse_id\":\"$A\",\"to_warehouse_id\":\"$B\",\"reason\":\"P0-03 修复验证调拨\",
  \"items\":[{\"material_id\":\"$MAT2\",\"from_location_id\":\"$LOCBUCKET\",\"qty\":40}]
}" > "$EV/raw/11_transfer.json"
TID=$(jq -r '.data.id' "$EV/raw/11_transfer.json")
echo "http=$HTTP no=$(jq -r '.data.transfer_no' "$EV/raw/11_transfer.json") id=$TID"
echo "$TID" > "$EV/raw/transfer2.id"

api PATCH "/business/erp/transfer/$TID/status" "$ADMIN" '{"status":"PENDING"}' > /dev/null
echo "submit http=$HTTP"
api GET '/business/flow/task/todo?page=1&page_size=20' "$LEAD" > "$EV/raw/11_todo.json"
TASK=$(jq -r --arg no "$(jq -r '.data.transfer_no' "$EV/raw/11_transfer.json")" '.data.list[] | select(.title|test($no)) | .id' "$EV/raw/11_todo.json")
api POST "/business/flow/task/$TASK/approve" "$LEAD" '{"comment":"P0-03 修复验证：同意调拨"}' > "$EV/raw/11_approve.json"
echo "approve http=$HTTP $(jq -c '{code,msg}' "$EV/raw/11_approve.json")"
sleep 1
api PATCH "/business/erp/transfer/$TID/status" "$ADMIN" '{"status":"IN_TRANSIT"}' > /dev/null; echo "transit http=$HTTP"
api PATCH "/business/erp/transfer/$TID/status" "$ADMIN" '{"status":"DONE"}' > "$EV/raw/11_done.json"; echo "done http=$HTTP $(jq -c '{code,msg}' "$EV/raw/11_done.json")"
sleep 1
api GET "/business/erp/transfer/$TID" "$ADMIN" | jq -c '{status:.data.status,qty:.data.total_qty,amount:.data.total_amount}'

echo "===== 4 桶级对账（期望：两侧都只动汇总桶，逐桶 0 差异）====="
ssh root@172.18.10.145 "export PGPASSWORD=$PG_PASS; psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X -c \"
WITH c AS (SELECT COALESCE(il.location_id::text,'(未分配)') AS loc, SUM(i.qty) AS cost_qty, SUM(i.amount) AS cost_amt
             FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id
            WHERE i.material_id='$MAT2'::uuid GROUP BY 1),
     l AS (SELECT COALESCE(location_id::text,'(未分配)') AS loc, SUM(qty) AS loc_qty
             FROM biz_inventory_location WHERE material_id='$MAT2'::uuid GROUP BY 1)
SELECT COALESCE(c.loc,l.loc) AS bucket, COALESCE(c.cost_qty,0) AS cost_qty, COALESCE(l.loc_qty,0) AS loc_qty,
       COALESCE(c.cost_qty,0)-COALESCE(l.loc_qty,0) AS diff
  FROM c FULL JOIN l ON c.loc=l.loc ORDER BY 1\""
