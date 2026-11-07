#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
LEAD=$(cat "$EV/raw/demo_lead.token")
TID=$(cat "$EV/raw/transfer.id")
TASK=$(jq -r '.data.list[0].id' "$EV/raw/05_todo.json")

snap() { # $1=label
  ssh root@172.18.10.145 "export PGPASSWORD=$PG_PASS; psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -X -c \"select i.material_id, w.warehouse_code, coalesce(il.location_id::text,'(unassigned)') as loc_dict, i.qty, i.avg_cost, i.amount from biz_inventory i join biz_warehouse w on w.id=i.warehouse_id left join biz_inventory_location il on il.id=i.location_id where i.material_id='$(cat $EV/raw/material.id)'::uuid order by w.warehouse_code, loc_dict\" -c \"select coalesce(sum(qty),0) as total_qty from biz_inventory where material_id='$(cat $EV/raw/material.id)'::uuid\""
  echo "---- $1 ----" >> "$EV/raw/05_snapshots.txt"
}

echo "===== ④-3 审批通过（demo_lead）====="
api POST "/business/flow/task/$TASK/approve" "$LEAD" '{"comment":"P0-03 库存演练：同意调拨"}' > "$EV/raw/05_approve.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/05_approve.json")"
sleep 1
api GET "/business/erp/transfer/$TID" "$ADMIN" > "$EV/raw/05_t_after_approve.json"
echo "status=$(jq -r '.data.status' "$EV/raw/05_t_after_approve.json")"

echo "===== 调拨前库存快照（DONE 之前）====="
snap before | tee "$EV/raw/05_inv_before.txt"
{ echo "== before DONE =="; cat "$EV/raw/05_inv_before.txt"; } >> "$EV/raw/05_snapshots.txt"

echo "===== ④-4 APPROVED→IN_TRANSIT ====="
api PATCH "/business/erp/transfer/$TID/status" "$ADMIN" '{"status":"IN_TRANSIT"}' > "$EV/raw/05_t_transit.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/05_t_transit.json")"
api GET "/business/erp/transfer/$TID" "$ADMIN" | jq -c '{status:.data.status, transit_warehouse_id:.data.transit_warehouse_id, shipped_at:.data.shipped_at}'

echo "===== ④-5 IN_TRANSIT→DONE（落账）====="
api PATCH "/business/erp/transfer/$TID/status" "$ADMIN" '{"status":"DONE"}' > "$EV/raw/05_t_done.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/05_t_done.json")"
sleep 1
api GET "/business/erp/transfer/$TID" "$ADMIN" > "$EV/raw/05_t_final.json"
jq -c '{status:.data.status,total_qty:.data.total_qty,total_amount:.data.total_amount,received_at:.data.received_at}' "$EV/raw/05_t_final.json"

echo "===== 调拨后库存快照 ====="
snap after | tee "$EV/raw/05_inv_after.txt"
{ echo "== after DONE =="; cat "$EV/raw/05_inv_after.txt"; } >> "$EV/raw/05_snapshots.txt"
