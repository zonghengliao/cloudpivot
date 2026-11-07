#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")

MAT_EX=$(jq -r '.data.id // empty' "$EV/raw/02_material.json" 2>/dev/null || true)
if [ -z "$MAT_EX" ]; then
  echo "===== ① 物料 DEMO-INV01 ====="
  api POST '/business/erp/material' "$ADMIN" '{
    "material_code":"DEMO-INV01",
    "material_name":"库存演练物料",
    "spec":"INV-DRILL",
    "unit_id":"68725745-ee24-4d9d-8ede-8fd28e6068f0",
    "material_type":"RAW",
    "costing_method":"MOVING_AVG",
    "tax_rate":0,
    "safety_stock":0,
    "status":1
  }' > "$EV/raw/02_material.json"
  echo "http=$HTTP"
  jq -c '{code,msg,id:.data.id,code_:.data.material_code}' "$EV/raw/02_material.json"
fi
MAT=$(jq -r '.data.id' "$EV/raw/02_material.json"); echo "$MAT" > "$EV/raw/material.id"

mkwh() { # $1=code $2=name $3=outfile
  local ex
  ex=$(jq -r '.data.id // empty' "$3" 2>/dev/null || true)
  if [ -z "$ex" ]; then
    api POST '/business/erp/warehouse' "$ADMIN" "{\"warehouse_code\":\"$1\",\"warehouse_name\":\"$2\",\"warehouse_type\":\"NORMAL\",\"level\":1,\"status\":1}" > "$3"
    echo "$1 http=$HTTP $(jq -c '{code,msg,id:.data.id}' "$3")"
  else
    echo "$1 exists $ex"
  fi
}

echo "===== ① 仓库 A / B ====="
mkwh WH-DVA 库存演练仓A "$EV/raw/02_wh_a.json"
mkwh WH-DVB 库存演练仓B "$EV/raw/02_wh_b.json"
A=$(jq -r '.data.id' "$EV/raw/02_wh_a.json"); echo "$A" > "$EV/raw/wh_a.id"
B=$(jq -r '.data.id' "$EV/raw/02_wh_b.json"); echo "$B" > "$EV/raw/wh_b.id"

mkloc() { # $1=code $2=name $3=wh_id $4=outfile
  local ex
  ex=$(jq -r '.data.id // empty' "$4" 2>/dev/null || true)
  if [ -z "$ex" ]; then
    api POST '/business/erp/location' "$ADMIN" "{\"location_code\":\"$1\",\"location_name\":\"$2\",\"warehouse_id\":\"$3\",\"location_type\":\"STORAGE\",\"is_default\":true,\"status\":1}" > "$4"
    echo "$1 http=$HTTP $(jq -c '{code,msg,id:.data.id}' "$4")"
  else
    echo "$1 exists $ex"
  fi
}

echo "===== ① 库位（每仓 1 个，默认库位）====="
mkloc LOC-DVA-01 A仓一号位 "$A" "$EV/raw/02_loc_a.json"
mkloc LOC-DVB-01 B仓一号位 "$B" "$EV/raw/02_loc_b.json"
jq -r '.data.id' "$EV/raw/02_loc_a.json" > "$EV/raw/loc_a.id"
jq -r '.data.id' "$EV/raw/02_loc_b.json" > "$EV/raw/loc_b.id"
echo "material=$MAT whA=$A whB=$B"
