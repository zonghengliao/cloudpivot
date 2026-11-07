#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
MAT=$(cat "$EV/raw/material.id"); A=$(cat "$EV/raw/wh_a.id")

echo "===== ② 库存期初录入（2026-10，仓A，DEMO-INV01 100 @ 10）====="
api POST '/business/erp/opening-balance/inventory' "$ADMIN" "{
  \"period\":\"2026-10\",
  \"items\":[{\"material_id\":\"$MAT\",\"warehouse_id\":\"$A\",\"qty\":100,\"unit_cost\":10,\"remark\":\"P0-03 库存链演练期初\"}]
}" > "$EV/raw/03_opening.json"
echo "http=$HTTP"
jq -c '{code,msg,period:.data.period}' "$EV/raw/03_opening.json"
jq -c '.data.inventory[]? // .data.items[]? | {material_code,warehouse_code,qty,unit_cost,amount}' "$EV/raw/03_opening.json" 2>/dev/null || true
jq -c '.data.voucher' "$EV/raw/03_opening.json"
