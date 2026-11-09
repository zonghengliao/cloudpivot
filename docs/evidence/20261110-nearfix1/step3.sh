#!/usr/bin/env bash
# NEAR-FIX-1 实测 step3：收款退款单全链路（列表 → 新建 → 冲减 → 确认 → 送审 → 审批 → 自动过账 → 附件）
set -uo pipefail
BASE=http://127.0.0.1:8099/api
OUT=/root/.hermes/profiles/backend-developer/cache/scratch/nearfix1
TOKEN=$(cat "$OUT/token.txt")
source "$OUT/ids.txt"
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

echo "===== 13) 空列表（GET /receipt-refund/page）"
curl -s "$BASE/business/erp/receipt-refund/page" "${AUTH[@]}" \
  | jq -c '{code,total:.data.total,page:.data.page,page_size:.data.page_size}'

echo
echo "===== 14) 前端校验口径的两条负例（应当 400，不是 500）"
echo -n "  退款原因只有 2 个字 → "
curl -s -X POST "$BASE/business/erp/receipt-refund" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"amount\":100,\"reason\":\"退货\"}" \
  | jq -c '{code,msg}'
echo -n "  退款金额 0 → "
curl -s -X POST "$BASE/business/erp/receipt-refund" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"amount\":0,\"reason\":\"零元退款测试\"}" \
  | jq -c '{code,msg}'
echo -n "  退款金额超过来源收款单（1000.01 > 1000）→ "
curl -s -X POST "$BASE/business/erp/receipt-refund" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"source_receipt_id\":\"$RCID\",\"amount\":1000.01,\"reason\":\"超额退款测试\"}" \
  | jq -c '{code,msg}'

echo
echo "===== 15) 新建退款单（来源收款单 RC-…0001，退款 400，原因 ≥4 字）"
RF=$(curl -s -X POST "$BASE/business/erp/receipt-refund" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"source_receipt_id\":\"$RCID\",\"amount\":400,\"method\":\"BANK_TRANSFER\",\"bank_account\":\"6222****0001\",\"reason\":\"客户部分退货，货款原路退回\"}")
echo "$RF" | jq -c '{code,msg,id:.data.id,no:.data.refund_no,status:.data.status,amount:.data.amount,alloc:.data.allocated_amount,src_no:.data.source_receipt_no,approval:.data.approval_status}'
RFID=$(jq -r '.data.id' <<<"$RF")
RNO=$(jq -r '.data.refund_no' <<<"$RF")
echo "RFID=$RFID" >> "$OUT/ids.txt"

echo
echo "===== 16) 列表复核：source_receipt_no 必须由后端 JOIN 带出（V0368 新列）"
curl -s "$BASE/business/erp/receipt-refund/page?refund_no=RR-" "${AUTH[@]}" \
  | jq -c '.data.list[] | {refund_no,source_receipt_id,source_receipt_no,customer_name,amount,status,status_label,approval_status,unallocated_amount}'

echo
echo "===== 17) 冲减（超可退 → 400；正常 400 → 200）"
echo -n "  冲减 1000 但该应收只收过 1000（可退 1000，但本单金额 400）→ 合计超额 400 拦："
curl -s -X POST "$BASE/business/erp/receipt-refund/$RFID/allocate" "${AUTH[@]}" \
  -d "{\"allocations\":[{\"receivable_id\":\"$ARID\",\"amount\":401}]}" | jq -c '{code,msg}'
echo -n "  正常冲减 400 → "
AL=$(curl -s -X POST "$BASE/business/erp/receipt-refund/$RFID/allocate" "${AUTH[@]}" \
  -d "{\"allocations\":[{\"receivable_id\":\"$ARID\",\"amount\":400}]}")
echo "$AL" | jq -c '{code,alloc:.data.allocated_amount,unalloc:.data.unallocated_amount,allocs:[.data.allocations[]|{ar_no,amount,ar_received,ar_status}]}'

echo
echo "===== 18) 应收已收 1000 → 600（退款冲减生效），状态 SETTLED → PARTIAL"
curl -s "$BASE/business/erp/receivable/$ARID" "${AUTH[@]}" \
  | jq -c '{ar_no:.data.ar_no,received:.data.received_amount,unsettled:.data.unsettled_amount,status:.data.status}'

echo
echo "===== 19) 未确认就过账（应当 409）、确认后再送审"
echo -n "  DRAFT 直接过账 → "
curl -s -X PATCH "$BASE/business/erp/receipt-refund/$RFID/status" "${AUTH[@]}" -d '{"status":"POSTED"}' \
  | jq -c '{code,msg}'
echo -n "  确认（DRAFT → CONFIRMED）→ "
curl -s -X PATCH "$BASE/business/erp/receipt-refund/$RFID/status" "${AUTH[@]}" -d '{"status":"CONFIRMED"}' \
  | jq -c '{code,status:.data.status}'
echo -n "  CONFIRMED 但未送审就过账（应 409 要求先有 APPROVED 实例）→ "
curl -s -X PATCH "$BASE/business/erp/receipt-refund/$RFID/status" "${AUTH[@]}" -d '{"status":"POSTED"}' \
  | jq -c '{code,msg}'

echo
echo "===== 20) 提交审批（POST /{id}/submit-approval）"
SB=$(curl -s -X POST "$BASE/business/erp/receipt-refund/$RFID/submit-approval" "${AUTH[@]}" -d '{}')
echo "$SB" | jq -c '{code,status:.data.status,approval_status:.data.approval_status,approval_no:.data.approval_instance_no,instance:.data.approval_instance_id}'
IID=$(jq -r '.data.approval.instance_id // .data.approval_instance_id' <<<"$SB")

echo
echo "===== 21) 审批人（admin 自审已由内控剔除？先看待办）"
TODO=$(curl -s "$BASE/business/flow/task/todo?page=1&page_size=20" "${AUTH[@]}")
echo "$TODO" | jq -c '{code,total:.data.total,tasks:[.data.list[]?|{id,task_id:.id,node:.current_node_name,status,biz_no:.biz_no}]}' 2>/dev/null || echo "$TODO" | head -c 600
TID=$(jq -r '[.data.list[]?|.id][0] // empty' <<<"$TODO")

if [ -n "${TID:-}" ]; then
  echo "  审批待办 task=$TID → approve"
  curl -s -X POST "$BASE/business/flow/task/$TID/approve" "${AUTH[@]}" -d '{"comment":"实测审批通过"}' \
    | jq -c '{code,msg,status:.data.status}' 2>/dev/null || true
else
  echo "  ⚠ 没有待办（内控排除了制单人自身的审批人身份）"
fi

echo
echo "===== 22) 详情复核（审批通过 → 自动过账 → POSTED + 退款凭证）"
sleep 1
DET=$(curl -s "$BASE/business/erp/receipt-refund/$RFID" "${AUTH[@]}")
echo "$DET" | jq -c '{code,no:.data.refund_no,status:.data.status,status_label:.data.status_label,voucher_no:.data.voucher_no,approval:.data.approval,allocations:[.data.allocations[]|{ar_no,amount}]}'

echo
echo "===== 23) 应收最终态（退款后应回 PARTIAL，已收 600）"
curl -s "$BASE/business/erp/receivable/$ARID" "${AUTH[@]}" \
  | jq -c '{ar_no:.data.ar_no,amount:.data.amount,received:.data.received_amount,unsettled:.data.unsettled_amount,status:.data.status}'

echo
echo "===== 24) 退款凭证分录（借 1122 应收 / 贷 1002 银行存款）"
VID=$(curl -s "$BASE/business/erp/receipt-refund/$RFID" "${AUTH[@]}" | jq -r '.data.voucher_id // empty')
if [ -n "${VID:-}" ]; then
  curl -s "$BASE/business/erp/voucher/$VID" "${AUTH[@]}" \
    | jq -c '{voucher_no:.data.voucher_no,source_type:.data.source_type,source_no:.data.source_no,status:.data.status,lines:[.data.entries[]?|{summary,debit:.debit_amount,credit:.credit_amount,account:.account_code}]}'
else
  echo "  （没有凭证 id：单据未过账）"
fi

echo
echo "===== 25) 退款凭据附件（doc_type=RECEIPT_REFUND —— V0368 把 CHECK 放宽到 17 值）"
curl -s -X POST "$BASE/business/attachment/upload" -H "Authorization: Bearer $TOKEN" \
  -F "doc_type=RECEIPT_REFUND" -F "doc_id=$RFID" -F "file_role=BANK_RECEIPT" \
  -F "remark=退款凭据实测" -F "file=@$OUT/bank-receipt.pdf;type=application/pdf" \
  | jq -c '{code,msg,id:.data.id,name:.data.file_name,role:.data.file_role}'
curl -s "$BASE/business/attachment/list?doc_type=RECEIPT_REFUND&doc_id=$RFID" "${AUTH[@]}" \
  | jq -c '{code,total:.data.total,items:[.data.items[]?|{file_name,file_role,uploader_name}]}' 2>/dev/null || true

echo
echo "===== 26) 作废链路（再建一张 DRAFT → 作废；再验证 POSTED 不能作废）"
R2=$(curl -s -X POST "$BASE/business/erp/receipt-refund" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"amount\":50,\"reason\":\"作废链路实测\"}")
R2ID=$(jq -r '.data.id' <<<"$R2"); R2NO=$(jq -r '.data.refund_no' <<<"$R2")
echo "  新建 $R2NO（DRAFT）"
echo -n "  作废原因 2 个字 → "
curl -s -X PATCH "$BASE/business/erp/receipt-refund/$R2ID/status" "${AUTH[@]}" -d '{"status":"CANCELLED","reason":"取消"}' | jq -c '{code,msg}'
echo -n "  作废原因足字 → "
curl -s -X PATCH "$BASE/business/erp/receipt-refund/$R2ID/status" "${AUTH[@]}" -d '{"status":"CANCELLED","reason":"客户取消本次退款"}' | jq -c '{code,status:.data.status,status_label:.data.status_label}'
echo -n "  已过账的 $RNO 再作废 → "
curl -s -X PATCH "$BASE/business/erp/receipt-refund/$RFID/status" "${AUTH[@]}" -d '{"status":"CANCELLED","reason":"试图作废已过账单据"}' | jq -c '{code,msg}'

echo
echo "===== 27) 列表最终态"
curl -s "$BASE/business/erp/receipt-refund/page" "${AUTH[@]}" \
  | jq -c '{code,total:.data.total,list:[.data.list[]|{refund_no,source_receipt_no,customer_name,amount,allocated_amount,status,approval_status}]}'
