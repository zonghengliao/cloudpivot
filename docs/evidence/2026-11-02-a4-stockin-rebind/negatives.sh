#!/usr/bin/env bash
# A4 真机反例/无源单验证（隔离实例 127.0.0.1:18091）
set -uo pipefail
BASE=http://127.0.0.1:18091/api
OUT=/root/a4-e2e
SUP=a4000000-0000-4000-8000-000000000002
WH=a4000000-0000-4000-8000-000000000003
MAT=a4000000-0000-4000-8000-000000000011
TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' | jq -r .data.access_token)
H="Authorization: Bearer $TOKEN"
POID=$(jq -r .data.order.id $OUT/03-po-create.json)
ITEM=$(jq -r '.data.items[0].item_id' $OUT/05-pending.json)

echo "===== R1 无源单没打标记 → 400 + 66027（PURCHASE 必须绑采购单）"
curl -s -X POST "$BASE/business/erp/stock-in" -H "$H" -H 'Content-Type: application/json' -d "{
  \"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":1,\"price_excl\":2,\"tax_rate\":13}]
}" | jq -c '{code,msg}' | tee $OUT/20-r1-no-source.json

echo "===== R2 无源单打了标记 + 写原因 → 200（E4：期初 / 复制 / 其他）"
curl -s -X POST "$BASE/business/erp/stock-in" -H "$H" -H 'Content-Type: application/json' -d "{
  \"is_sourceless\":true,\"supplier_id\":\"$SUP\",\"warehouse_id\":\"$WH\",
  \"remark\":\"期初补录：2026-10 前的手工库存\",
  \"items\":[{\"material_id\":\"$MAT\",\"qty\":1,\"price_excl\":2,\"tax_rate\":13}]
}" > $OUT/21-r2-sourceless.json
jq -c '{code,msg,no:.data.header.in_no,is_sourceless:.data.header.is_sourceless,supplier_name:.data.header.supplier_name}' $OUT/21-r2-sourceless.json

echo "===== R3 有源单 + 供应商填错 → 400 + 66028（不静默覆盖）"
curl -s -X POST "$BASE/business/erp/stock-in" -H "$H" -H 'Content-Type: application/json' -d "{
  \"source_order_id\":\"$POID\",\"supplier_id\":\"a4000000-0000-4000-8000-0000000000ff\",
  \"warehouse_id\":\"$WH\",
  \"items\":[{\"purchase_order_item_id\":\"$ITEM\",\"material_id\":\"$MAT\",\"qty\":1,\"price_excl\":2,\"tax_rate\":13}]
}" | jq -c '{code,msg}' | tee $OUT/22-r3-mismatch.json

echo "===== R4 复制为新单 → is_sourceless=true + 备注留「复制自 IN-xxx」"
ORIG=$(jq -r .data.header.id $OUT/06-stockin-create.json)
curl -s -X POST "$BASE/business/erp/stock-in/$ORIG/copy" -H "$H" > $OUT/23-r4-copy.json
jq -c '{code,msg,no:.data.header.in_no,is_sourceless:.data.header.is_sourceless,source_order_id:.data.header.source_order_id,remark:.data.header.remark}' $OUT/23-r4-copy.json

echo "===== R5 列表（含无源单那条）"
curl -s "$BASE/business/erp/stock-in/page?page=1&page_size=10" -H "$H" \
  | jq -c '{code,total:.data.total,rows:[.data.list[]|{in_no,is_sourceless,supplier_name,source_order_no,status}]}' | tee $OUT/24-r5-list.json
