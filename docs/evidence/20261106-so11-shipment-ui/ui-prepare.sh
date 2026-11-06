#!/usr/bin/env bash
# SO-11 真机 UI 验收的前置造数：干净夹具 + 期初库存 + 一张 APPROVED 销售订单
# 幂等：先按 SO11UI 备注前缀清干净，再重建（每次重跑都从同一状态开始）
set -uo pipefail
export PGPASSWORD=alun
PSQL="psql -h 127.0.0.1 -U alun -d ${DB:-cloudpivot_so11} -tAq"
BASE=${BASE:-http://127.0.0.1:18099}
TAG='SO11UI'
WH=${WH:-d1370001-0000-4000-8000-000000000001}
MAT_A=${MAT_A:-d1370003-0000-4000-8000-000000000002}
CUST=${CUST:-d0260000-0000-4000-8000-000000000001}

$PSQL <<SQL
DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN (
    SELECT id FROM biz_stock_out WHERE source_id IN (
        SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id
         WHERE o.remark LIKE '$TAG%'));
DELETE FROM biz_status_log WHERE
    (entity_type = 'STOCK_OUT' AND entity_id IN (
        SELECT id FROM biz_stock_out WHERE source_id IN (
            SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id
             WHERE o.remark LIKE '$TAG%')))
 OR (entity_type = 'SHIPMENT' AND entity_id IN (
        SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id
         WHERE o.remark LIKE '$TAG%'))
 OR (entity_type = 'SALE_ORDER' AND entity_id IN (
        SELECT id FROM biz_sales_order WHERE remark LIKE '$TAG%'));
DELETE FROM biz_stock_out WHERE source_id IN (
    SELECT s.id FROM biz_shipment s JOIN biz_sales_order o ON o.id = s.so_id
     WHERE o.remark LIKE '$TAG%');
DELETE FROM biz_shipment WHERE so_id IN (SELECT id FROM biz_sales_order WHERE remark LIKE '$TAG%');
DELETE FROM biz_sales_order WHERE remark LIKE '$TAG%';
DELETE FROM biz_inventory_txn WHERE material_id = '$MAT_A';
DELETE FROM biz_inventory WHERE material_id = '$MAT_A';
DELETE FROM biz_inventory_location WHERE material_id = '$MAT_A';
INSERT INTO biz_inventory (material_id, warehouse_id, qty, amount, avg_cost)
VALUES ('$MAT_A','$WH', 10, 100, 10);
INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty)
VALUES ('$MAT_A','$WH', NULL, 10);
SQL

TOKEN=${ACCESS_TOKEN:-}
if [ -z "$TOKEN" ]; then
  TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"Admin@123456"}' | jq -r '.data.access_token')
  if [ "$TOKEN" = "null" ] || [ -z "$TOKEN" ]; then
    TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
      -d '{"username":"admin","password":"Admin@20261106"}' | jq -r '.data.access_token')
  fi
fi
[ "$TOKEN" != "null" ] && [ -n "$TOKEN" ] || { echo 'login failed'; exit 1; }

ORDER=$(curl -s -X POST "$BASE/api/business/erp/sales-order" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$CUST\",\"order_date\":\"2026-11-06\",\"remark\":\"$TAG 页面验证\",
\"items\":[{\"material_id\":\"$MAT_A\",\"qty\":10,\"price\":100,\"tax_rate\":13}]}")
SO_ID=$(printf '%s' "$ORDER" | jq -r '.data.id')
SO_NO=$(printf '%s' "$ORDER" | jq -r '.data.so_no')
[ "$SO_ID" != "null" ] || { echo "order failed: $ORDER"; exit 1; }
$PSQL -c "UPDATE biz_sales_order SET status='APPROVED', updated_at=now() WHERE id='$SO_ID'" > /dev/null

printf 'SO_ID=%s\nSO_NO=%s\nMAT_A=%s\n' "$SO_ID" "$SO_NO" "$MAT_A"
jq -n --arg so_id "$SO_ID" --arg so_no "$SO_NO" --arg material_id "$MAT_A" \
      --arg material_label 'M-W1A-002' \
      '{so_id: $so_id, so_no: $so_no, material_id: $material_id, material_label: $material_label}' \
      > /root/.hermes/profiles/frontend-developer/cache/scratch/so11-verify/fixture.json
