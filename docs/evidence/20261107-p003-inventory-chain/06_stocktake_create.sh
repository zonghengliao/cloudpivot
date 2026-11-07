#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
A=$(cat "$EV/raw/wh_a.id")

echo "===== ⑤-1 建全盘单（仓 A）====="
api POST '/business/erp/stock-take' "$ADMIN" "{
  \"warehouse_id\":\"$A\",\"take_type\":\"FULL\",\"remark\":\"P0-03 库存演练：盘盈 5 件\"
}" > "$EV/raw/06_st_gain.json"
echo "http=$HTTP"
jq -c '{code,msg,id:.data.id,no:.data.take_no,status:.data.status,total_book_qty:.data.total_book_qty}' "$EV/raw/06_st_gain.json"
jq -c '.data.items[]? | {item_id:.id, material_code, book_qty, actual_qty, diff_qty, location_id}' "$EV/raw/06_st_gain.json"
