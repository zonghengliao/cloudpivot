#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
MAT=$(cat "$EV/raw/material.id"); A=$(cat "$EV/raw/wh_a.id")

echo "===== ⑥-1 建出库单（仓 A，领用 20 件）====="
api POST '/business/erp/stock-out' "$ADMIN" "{
  \"out_type\":\"ISSUE\",\"warehouse_id\":\"$A\",\"reason\":\"P0-03 库存链演练：领用出库\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":20}]
}" > "$EV/raw/08_stockout.json"
echo "http=$HTTP"
jq -c '{code,msg,id:.data.id,no:(.data.out_no // .data.header.out_no),status:(.data.status // .data.header.status),qty:(.data.total_qty // .data.header.total_qty),amount:(.data.total_amount // .data.header.total_amount)}' "$EV/raw/08_stockout.json"
jq -c '.' "$EV/raw/08_stockout.json" | head -c 600
