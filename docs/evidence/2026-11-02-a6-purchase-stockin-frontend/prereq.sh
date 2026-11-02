#!/bin/bash
# A6 前置验证（卡面要求）：真实 token 打三条接口，任一条不通就停。
# token 只在脚本内流转，不落盘打印。
set -uo pipefail
BASE=http://127.0.0.1:18086/api
OUT="$(dirname "$0")/raw"
mkdir -p "$OUT"

login() {
  curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$1\"}"
}

for pw in 'A5@Verify2026' 'Admin@123456' 'A6@Verify2026'; do
  BODY=$(login "$pw")
  TOK=$(printf '%s' "$BODY" | jq -r '.data.access_token // empty')
  if [ -n "$TOK" ] && [ "$TOK" != "null" ]; then
    echo "login ok with pw=$pw"
    if [ "$pw" = 'Admin@123456' ]; then
      echo "--- 首次登录需改密 ---"
      curl -s -o /dev/null -w 'PUT password -> %{http_code}\n' -X PUT "$BASE/system/auth/password" \
        -H "Authorization: Bearer $TOK" -H 'Content-Type: application/json' \
        -d '{"old_password":"Admin@123456","new_password":"A6@Verify2026"}'
      TOK=$(login 'A6@Verify2026' | jq -r '.data.access_token // empty')
      echo "relogin token_len=${#TOK}"
    fi
    break
  fi
done
[ -n "${TOK:-}" ] && [ "$TOK" != "null" ] || { echo "LOGIN FAILED"; exit 1; }
printf '%s' "$TOK" > "$OUT/token.txt"
AUTH="Authorization: Bearer $TOK"

echo
echo "=== ① GET /erp/contract/page ==="
curl -s -o "$OUT/contract-page.json" -w 'http=%{http_code}\n' \
  "$BASE/business/erp/contract/page?page=1&page_size=20&contract_type=PURCHASE" -H "$AUTH"
jq -c '{code, total: .data.total, rows: [.data.list[]? | {id, contract_no, status, contract_type, party_b_name}]}' "$OUT/contract-page.json"

ACTIVE_ID=$(jq -r '[.data.list[]? | select(.status=="ACTIVE" or .status=="SIGNED")][0].id // empty' "$OUT/contract-page.json")
if [ -z "$ACTIVE_ID" ]; then
  echo "--- 没有可下单合同：新建一张并走 DRAFT→SIGNED→ACTIVE ---"
  SUPPLIER=$(curl -s "$BASE/business/erp/supplier/page?page=1&page_size=1" -H "$AUTH" | jq -r '.data.list[0].id')
  TODAY=$(date +%F)
  NEXT_YEAR=$(date -d '+1 year' +%F)
  NEW=$(curl -s -X POST "$BASE/business/erp/contract" -H "$AUTH" -H 'Content-Type: application/json' -d "{
    \"contract_name\": \"A6 真机验收采购合同\",
    \"contract_type\": \"PURCHASE\",
    \"party_b_id\": \"$SUPPLIER\",
    \"amount\": 500000,
    \"sign_date\": \"$TODAY\",
    \"start_date\": \"$TODAY\",
    \"end_date\": \"$NEXT_YEAR\"
  }")
  echo "$NEW" | jq -c '{code, id: .data.id, no: .data.contract_no, status: .data.status, msg}'
  CID=$(printf '%s' "$NEW" | jq -r '.data.id // empty')
  [ -n "$CID" ] || { echo "CREATE CONTRACT FAILED"; exit 1; }
  for target in SIGNED ACTIVE; do
    curl -s -o "$OUT/contract-$target.json" -w "PATCH status=$target -> %{http_code}\n" \
      -X PATCH "$BASE/business/erp/contract/$CID/status" -H "$AUTH" -H 'Content-Type: application/json' \
      -d "{\"status\":\"$target\"}"
  done
  ACTIVE_ID="$CID"
  curl -s -o "$OUT/contract-page.json" "$BASE/business/erp/contract/page?page=1&page_size=20&contract_type=PURCHASE" -H "$AUTH"
fi
echo "use contract_id=$ACTIVE_ID"
printf '%s' "$ACTIVE_ID" > "$OUT/contract_id.txt"

echo
echo "=== ② POST /erp/purchase-order（CONTRACT 型，带 contract_id，**不带 supplier_id**）==="
WS=$(curl -s "$BASE/business/erp/warehouse/page?page=1&page_size=5" -H "$AUTH" | jq -r '.data.list[0].id')
MAT=$(curl -s "$BASE/business/erp/material/page?page=1&page_size=5" -H "$AUTH" | jq -r '.data.list[0].id')
echo "warehouse=$WS material=$MAT"
PO=$(curl -s -o "$OUT/po-create.json" -w '%{http_code}' -X POST "$BASE/business/erp/purchase-order" \
  -H "$AUTH" -H 'Content-Type: application/json' -d "{
    \"purchase_type\": \"CONTRACT\",
    \"contract_id\": \"$ACTIVE_ID\",
    \"warehouse_id\": \"$WS\",
    \"order_date\": \"$(date +%F)\",
    \"price_mode\": \"EXCL\",
    \"remark\": \"A6 前置验证：CONTRACT 型采购单\",
    \"items\": [{\"material_id\": \"$MAT\", \"qty\": 2, \"price_excl\": 50, \"tax_rate\": 13}]
  }")
echo "purchase-order POST -> $PO"
jq -c '{code, msg, order_no: .data.order.order_no, purchase_type: .data.order.purchase_type,
        contract_id: .data.order.contract_id, contract_no_snapshot: .data.order.contract_no_snapshot,
        contract_no: .data.order.contract_no, supplier_id: .data.order.supplier_id,
        supplier_name: .data.order.supplier_name}' "$OUT/po-create.json"

PO_ID=$(jq -r '.data.order.id // empty' "$OUT/po-create.json")
printf '%s' "$PO_ID" > "$OUT/po_id.txt"

echo
echo "=== ③ 审批 + 收货：把这张单推进到 APPROVED，再验 POST /erp/stock-in（不带 supplier_id）==="
curl -s -o "$OUT/po-submit.json" -w 'submit-approval -> %{http_code}\n' -X POST \
  "$BASE/business/erp/purchase-order/$PO_ID/submit-approval" -H "$AUTH" -H 'Content-Type: application/json' -d '{}'
jq -c '{code, msg, status: .data.order.status, approval_instance_no: .data.order.approval_instance_no}' "$OUT/po-submit.json"
