#!/usr/bin/env bash
# NEAR-FIX-1 实测 step2：收款单上传银行回单 → 确认 → 过账
set -uo pipefail
BASE=http://127.0.0.1:8099/api
OUT=/root/.hermes/profiles/backend-developer/cache/scratch/nearfix1
TOKEN=$(cat "$OUT/token.txt")
source "$OUT/ids.txt"
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

echo "===== 9) 上传银行回单（RECEIPT / $RCID / BANK_RECEIPT）"
UP=$(curl -s -X POST "$BASE/business/attachment/upload" -H "Authorization: Bearer $TOKEN" \
  -F "doc_type=RECEIPT" -F "doc_id=$RCID" -F "file_role=BANK_RECEIPT" \
  -F "remark=NEAR-FIX1 实测回单" -F "file=@$OUT/bank-receipt.pdf;type=application/pdf")
echo "$UP" | jq -c '{code,msg,id:.data.id,name:.data.file_name,role:.data.file_role}' 2>/dev/null || echo "$UP"

echo "===== 10) 确认收款（应为 200 / CONFIRMED）"
CF=$(curl -s -X PATCH "$BASE/business/erp/receipt/$RCID/status" "${AUTH[@]}" -d '{"status":"CONFIRMED"}')
echo "$CF" | jq -c '{code,msg,status:.data.status,allocated:.data.allocated_amount}'

echo "===== 11) 过账收款（CONFIRMED → POSTED，生成借 1002 / 贷 1122 凭证）"
PS=$(curl -s -X PATCH "$BASE/business/erp/receipt/$RCID/status" "${AUTH[@]}" -d '{"status":"POSTED"}')
echo "$PS" | jq -c '{code,msg,status:.data.status,voucher_no:.data.voucher_no}'

echo "===== 12) 复核应收已收（应为 1000 / SETTLED）"
curl -s "$BASE/business/erp/receivable/$ARID" "${AUTH[@]}" \
  | jq -c '{code,ar_no:.data.ar_no,received:.data.received_amount,unsettled:.data.unsettled_amount,status:.data.status}'
