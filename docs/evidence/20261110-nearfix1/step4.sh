#!/usr/bin/env bash
# NEAR-FIX-1 实测 step4：建财务经理账号 → 送审 → 跨人审批 → 自动过账 → 退款凭据附件
set -uo pipefail
BASE=http://127.0.0.1:8099/api
OUT=/root/.hermes/profiles/backend-developer/cache/scratch/nearfix1
TOKEN=$(cat "$OUT/token.txt")
CID=$(grep -o 'CID=[^ ]*' "$OUT/ids.txt" | cut -d= -f2)
ARID=$(grep -o 'ARID=[^ ]*' "$OUT/ids.txt" | cut -d= -f2)
RCID=$(grep -o 'RCID=[^ ]*' "$OUT/ids.txt" | cut -d= -f2)
RFID=$(grep -o 'RFID=[^ ]*' "$OUT/ids.txt" | tail -1 | cut -d= -f2)
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

FM_ROLE=$(PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d nearfix2 -tAc \
  "select id from sys_role where role_code='FINANCE_MANAGER' and deleted_at is null")
echo "FINANCE_MANAGER role=$FM_ROLE"

echo "===== 28) 建财务经理账号 finance01 并授 FINANCE_MANAGER（内控「制单人 ≠ 审批人」）"
U=$(curl -s -X POST "$BASE/system/user" "${AUTH[@]}" \
  -d "{\"username\":\"finance01\",\"password\":\"Finance@123456\",\"real_name\":\"财务经理 实测\",\"status\":1,\"role_ids\":[\"$FM_ROLE\"]}")
echo "$U" | jq -c '{code,msg,id:.data.id,username:.data.username}' 2>/dev/null || echo "$U"

echo
echo "===== 29) finance01 登录"
FL=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"finance01","password":"Finance@123456"}')
FTOKEN=$(jq -r '.data.access_token // empty' <<<"$FL")
echo "finance token 长度 ${#FTOKEN}"
if [ -z "$FTOKEN" ]; then echo "$FL" | head -c 400; exit 1; fi

echo
echo "===== 30) admin 重新送审（上次 65018 = 派不出审批人；现在财务经理有了）"
SB=$(curl -s -X POST "$BASE/business/erp/receipt-refund/$RFID/submit-approval" "${AUTH[@]}" -d '{}')
echo "$SB" | jq -c '{code,msg,status:.data.status,approval_status:.data.approval_status,approval_no:.data.approval_instance_no,approval:.data.approval}' 2>/dev/null || echo "$SB" | head -c 400

echo
echo "===== 31) finance01 的待办"
TODO=$(curl -s "$BASE/business/flow/task/todo?page=1&page_size=20" -H "Authorization: Bearer $FTOKEN")
echo "$TODO" | jq -c '{code,total:.data.total,tasks:[.data.list[]?|{id,node:.current_node_name,status,biz_type,biz_no}]}' 2>/dev/null || echo "$TODO" | head -c 500
TID=$(jq -r '[.data.list[]?|.id][0] // empty' <<<"$TODO")

if [ -n "${TID:-}" ]; then
  echo "  approve task=$TID"
  curl -s -X POST "$BASE/business/flow/task/$TID/approve" -H "Authorization: Bearer $FTOKEN" \
    -H 'Content-Type: application/json' -d '{"comment":"实测审批通过：退款凭据已核"}' \
    | jq -c '{code,msg}' 2>/dev/null
else
  echo "  ⚠ 仍无待办"
fi

echo
echo "===== 32) 详情：审批通过 → 自动过账（POSTED + 退款凭证号）"
sleep 2
DET=$(curl -s "$BASE/business/erp/receipt-refund/$RFID" "${AUTH[@]}")
echo "$DET" | jq -c '{code,no:.data.refund_no,status:.data.status,status_label:.data.status_label,voucher_no:.data.voucher_no,voucher_id:.data.voucher_id,approval:.data.approval,alloc:[.data.allocations[]|{ar_no,amount,ar_received,ar_status}]}'

echo
echo "===== 33) 应收最终态（退款 400 后：已收 600 / 未收 400 / PARTIAL）"
curl -s "$BASE/business/erp/receivable/$ARID" "${AUTH[@]}" \
  | jq -c '{ar_no:.data.ar_no,amount:.data.amount,received:.data.received_amount,unsettled:.data.unsettled_amount,status:.data.status}'

echo
echo "===== 34) 退款凭证分录（借 1122 应收 / 贷 1002 银行存款，各 400）"
VID=$(jq -r '.data.voucher_id // empty' <<<"$DET")
if [ -n "${VID:-}" ]; then
  V=$(curl -s "$BASE/business/erp/voucher/$VID" "${AUTH[@]}")
  echo "$V" | jq -c '{voucher_no:.data.voucher_no,source_type:.data.source_type,source_no:.data.source_no,status:.data.status,total_debit:.data.total_debit,total_credit:.data.total_credit}'
  echo "$V" | jq -c '.data.entries[]? // .data.lines[]? | {account:.account_code,account_name:.account_name,debit:.debit_amount,credit:.credit_amount,summary}'
else
  echo "（没有凭证 id）"
fi

echo
echo "===== 35) 退款凭据附件（doc_type=RECEIPT_REFUND —— V0368 放宽 CHECK 到 17 值 + attachment::model::DocKind 17）"
UP=$(curl -s -X POST "$BASE/business/attachment/upload" -H "Authorization: Bearer $TOKEN" \
  -F "doc_type=RECEIPT_REFUND" -F "doc_id=$RFID" -F "file_role=BANK_RECEIPT" \
  -F "remark=退款凭据实测" -F "file=@$OUT/bank-receipt.pdf;type=application/pdf")
echo "$UP" | jq -c '{code,msg,id:.data.id,file_name:.data.file_name,file_role:.data.file_role}' 2>/dev/null || echo "$UP" | head -c 400
echo -n "  list: "
curl -s "$BASE/business/attachment/list?doc_type=RECEIPT_REFUND&doc_id=$RFID" "${AUTH[@]}" \
  | jq -c '{code,total:.data.total,items:[.data.items[]?|{file_name,file_role,uploader_name}]}' 2>/dev/null | head -c 500

echo
echo "===== 36) 作废链路（DRAFT → CANCELLED；已过账的不能作废）"
R2=$(curl -s -X POST "$BASE/business/erp/receipt-refund" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"amount\":50,\"reason\":\"作废链路实测\"}")
echo "$R2" | jq -c '{code,msg,id:.data.id,no:.data.refund_no,status:.data.status}' 2>/dev/null || echo "$R2" | head -c 300
R2ID=$(jq -r '.data.id // empty' <<<"$R2")
if [ -n "${R2ID:-}" ]; then
  echo -n "  作废原因 2 个字 → "
  curl -s -X PATCH "$BASE/business/erp/receipt-refund/$R2ID/status" "${AUTH[@]}" -d '{"status":"CANCELLED","reason":"取消"}' | jq -c '{code,msg}'
  echo -n "  作废原因足字 → "
  curl -s -X PATCH "$BASE/business/erp/receipt-refund/$R2ID/status" "${AUTH[@]}" -d '{"status":"CANCELLED","reason":"客户取消本次退款"}' | jq -c '{code,status:.data.status,status_label:.data.status_label}'
fi
echo -n "  已过账的 $RFID 再作废 → "
curl -s -X PATCH "$BASE/business/erp/receipt-refund/$RFID/status" "${AUTH[@]}" -d '{"status":"CANCELLED","reason":"试图作废已过账单据"}' | jq -c '{code,msg}'

echo
echo "===== 37) 列表最终态（含来源收款单号 / 审批状态）"
curl -s "$BASE/business/erp/receipt-refund/page" "${AUTH[@]}" \
  | jq -c '{code,total:.data.total,list:[.data.list[]|{refund_no,source_receipt_no,customer_name,amount,allocated_amount,status,approval_status}]}'
