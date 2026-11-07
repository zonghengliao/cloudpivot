#!/usr/bin/env bash
set -euo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/00_env.sh

# 管理员口令不落仓库：从 145 的 env 文件现取（也可外部 export ADMIN_PASS 覆盖）
: "${ADMIN_PASS:=$(ssh root@172.18.10.145 'grep -m1 ^NIK_ADMIN_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2')}"
ADMIN=$(login admin "$ADMIN_PASS")
echo "admin token len=${#ADMIN}"
echo "$ADMIN" > "$EV/raw/admin.token"

LEAD=$(login demo_lead 'Demo@123456')
echo "demo_lead token len=${#LEAD}"
echo "$LEAD" > "$EV/raw/demo_lead.token"

# 参考数据：单位 / 供应商 / 物料
api GET '/business/erp/unit/page?page=1&page_size=50' "$ADMIN" > "$EV/raw/units.json"
echo "units http=$HTTP"
jq -c '.data.records[]? | {unit_code, id}' "$EV/raw/units.json" 2>/dev/null || jq -c '.data' "$EV/raw/units.json" | head -c 800

api GET '/business/erp/supplier/page?page=1&page_size=20&keyword=DEMO' "$ADMIN" > "$EV/raw/suppliers.json"
echo "suppliers http=$HTTP"
jq -c '.data.records[]? | {supplier_code, supplier_name, id}' "$EV/raw/suppliers.json"

api GET '/business/erp/material/page?page=1&page_size=20&keyword=DEMO' "$ADMIN" > "$EV/raw/materials.json"
echo "materials http=$HTTP"
jq -c '.data.records[]? | {material_code, material_name, id, unit_id, tax_rate, costing_method}' "$EV/raw/materials.json"

api GET '/business/erp/warehouse/page?page=1&page_size=50' "$ADMIN" > "$EV/raw/warehouses.json"
echo "warehouses http=$HTTP"
jq -c '.data.records[]? | {warehouse_code, warehouse_name, id, level}' "$EV/raw/warehouses.json"
