#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
MAT=$(cat "$EV/raw/material.id"); A=$(cat "$EV/raw/wh_a.id"); B=$(cat "$EV/raw/wh_b.id")
LOCA=$(ssh root@172.18.10.145 "export PGPASSWORD=$PG_PASS; psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X -tAc \"select il.id from biz_inventory_location il join biz_warehouse w on w.id=il.warehouse_id where w.warehouse_code='WH-DVA' and il.location_id is not null limit 1\"")

echo "===== ④ 建调拨单（A→B，40 件；from_location=$LOCA）====="
api POST '/business/erp/transfer' "$ADMIN" "{
  \"from_warehouse_id\":\"$A\",\"to_warehouse_id\":\"$B\",\"reason\":\"P0-03 库存链演练调拨\",
  \"items\":[{\"material_id\":\"$MAT\",\"from_location_id\":\"$LOCA\",\"qty\":40}]
}" > "$EV/raw/05_transfer.json"
echo "http=$HTTP"
jq -c '{code,msg,id:.data.id,no:.data.transfer_no,status:.data.status,total_qty:.data.total_qty,total_amount:.data.total_amount}' "$EV/raw/05_transfer.json"
jq -r '.data.id // empty' "$EV/raw/05_transfer.json" > "$EV/raw/transfer.id"
jq -c '.data.items[]? | {line_no,material_id,qty,from_location_id,to_location_id}' "$EV/raw/05_transfer.json"
