#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
MAT=$(cat "$EV/raw/material.id"); A=$(cat "$EV/raw/wh_a.id")
SUP="01a1b5d2-504f-72f7-92b2-00f884285a2c"

echo "===== ③ 直接入库单（无采购来源，50 @ 12）：建单 ====="
api POST '/business/erp/stock-in' "$ADMIN" "{
  \"is_sourceless\":true,
  \"supplier_id\":\"$SUP\",
  \"warehouse_id\":\"$A\",
  \"remark\":\"P0-03 库存链演练：直接入库 50@12\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":50,\"price_excl\":12,\"tax_rate\":0}]
}" > "$EV/raw/04_stockin.json"
echo "http=$HTTP"
jq -c '{code,msg,id:.data.id,no:.data.in_no,status:.data.status,total_qty:.data.total_qty,total_amount:.data.total_amount,in_type:.data.in_type}' "$EV/raw/04_stockin.json"
IN_ID=$(jq -r '.data.id' "$EV/raw/04_stockin.json"); echo "$IN_ID" > "$EV/raw/stockin.id"

echo "===== ③ 提交过账 ====="
api POST "/business/erp/stock-in/$IN_ID/submit-post" "$ADMIN" '{}' > "$EV/raw/04_stockin_post.json"
echo "http=$HTTP"
jq -c '{code,msg,status:.data.status,approval:.data.approval_instance_id}' "$EV/raw/04_stockin_post.json"
