#!/usr/bin/env bash
# NEAR-FIX-1 实测 step6：撤回审批（前端「撤回」按钮对应的后端链路）
set -uo pipefail
BASE=http://127.0.0.1:8099/api
OUT=/root/.hermes/profiles/backend-developer/cache/scratch/nearfix1
TOKEN=$(cat "$OUT/token.txt")
CID=01a1c18d-787d-75f2-8396-eab063eb7b54
ARID=eb9579bc-8d03-4d1f-a8b4-d7a45d881302
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

echo "===== 44) 新建 → 冲减 → 确认 → 送审"
R=$(curl -s -X POST "$BASE/business/erp/receipt-refund" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"amount\":100,\"method\":\"CASH\",\"reason\":\"撤回链路实测\"}")
RID=$(jq -r '.data.id' <<<"$R"); RNO=$(jq -r '.data.refund_no' <<<"$R")
echo "  $RNO ($RID)"
curl -s -X POST "$BASE/business/erp/receipt-refund/$RID/allocate" "${AUTH[@]}" \
  -d "{\"allocations\":[{\"receivable_id\":\"$ARID\",\"amount\":100}]}" | jq -c '{code,alloc:.data.allocated_amount}'
curl -s -X PATCH "$BASE/business/erp/receipt-refund/$RID/status" "${AUTH[@]}" -d '{"status":"CONFIRMED"}' | jq -c '{code,status:.data.status}'
SB=$(curl -s -X POST "$BASE/business/erp/receipt-refund/$RID/submit-approval" "${AUTH[@]}" -d '{}')
echo "$SB" | jq -c '{code,approval_status:.data.approval_status,instance:.data.approval_instance_id,no:.data.approval_instance_no}'
IID=$(jq -r '.data.approval.instance_id // .data.approval_instance_id' <<<"$SB")

echo
echo "===== 45) 撤回（POST /business/flow/instance/{id}/withdraw）"
curl -s -X POST "$BASE/business/flow/instance/$IID/withdraw" "${AUTH[@]}" \
  -d '{"reason":"实测撤回审批"}' | jq -c '{code,msg}'

echo
echo "===== 46) 撤回后的详情：实例 WITHDRAWN、单据仍是 CONFIRMED、账没动"
sleep 1
DET=$(curl -s "$BASE/business/erp/receipt-refund/$RID" "${AUTH[@]}")
echo "$DET" | jq -c '{status:.data.status,status_label:.data.status_label,voucher_no:.data.voucher_no,approval:.data.approval}'
echo -n "  应收（应仍是已收 600，撤回不改账）: "
curl -s "$BASE/business/erp/receivable/$ARID" "${AUTH[@]}" | jq -c '{received:.data.received_amount,status:.data.status}'

echo
echo "===== 47) 撤回后可再送审（WITHDRAWN 不是 PENDING，前端 canSubmitApproval 会重新点亮）"
curl -s -X POST "$BASE/business/erp/receipt-refund/$RID/submit-approval" "${AUTH[@]}" -d '{}' \
  | jq -c '{code,approval_status:.data.approval_status,no:.data.approval_instance_no}'
