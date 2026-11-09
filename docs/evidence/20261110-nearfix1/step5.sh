#!/usr/bin/env bash
# NEAR-FIX-1 实测 step5：NULLIF 修复复验（不选退款方式也能建单）+ 作废链路 + 附件分栏
set -uo pipefail
BASE=http://127.0.0.1:8099/api
OUT=/root/.hermes/profiles/backend-developer/cache/scratch/nearfix1
TOKEN=$(cat "$OUT/token.txt")
RFID=$(grep -o 'RFID=[^ ]*' "$OUT/ids.txt" | tail -1 | cut -d= -f2)
CID=01a1c18d-787d-75f2-8396-eab063eb7b54
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

echo "===== 38) 修复复验：不给 method（可选列 → NULL，不再撞 ck_biz_receipt_refund_method）"
R2=$(curl -s -X POST "$BASE/business/erp/receipt-refund" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"amount\":50,\"reason\":\"作废链路实测\"}")
echo "$R2" | jq -c '{code,msg,no:.data.refund_no,status:.data.status,method:.data.method,method_label:.data.method_label}' 2>/dev/null || echo "$R2" | head -c 300
R2ID=$(jq -r '.data.id // empty' <<<"$R2")

if [ -n "${R2ID:-}" ]; then
  echo
  echo "===== 39) 作废：原因 2 个字 → 400；足字 → CANCELLED"
  echo -n "  2 个字 → "
  curl -s -X PATCH "$BASE/business/erp/receipt-refund/$R2ID/status" "${AUTH[@]}" \
    -d '{"status":"CANCELLED","reason":"取消"}' | jq -c '{code,msg}'
  echo -n "  足字   → "
  curl -s -X PATCH "$BASE/business/erp/receipt-refund/$R2ID/status" "${AUTH[@]}" \
    -d '{"status":"CANCELLED","reason":"客户取消本次退款"}' \
    | jq -c '{code,status:.data.status,status_label:.data.status_label}'
  echo -n "  再作废一次（终态不可再迁）→ "
  curl -s -X PATCH "$BASE/business/erp/receipt-refund/$R2ID/status" "${AUTH[@]}" \
    -d '{"status":"CANCELLED","reason":"终态重复作废"}' | jq -c '{code,msg}'
fi

echo
echo "===== 40) 已过账退款单不能作废（POSTED 是终态）"
curl -s -X PATCH "$BASE/business/erp/receipt-refund/$RFID/status" "${AUTH[@]}" \
  -d '{"status":"CANCELLED","reason":"试图作废已过账单据"}' | jq -c '{code,msg}'

echo
echo "===== 41) 退款凭据附件列表（分栏 + 上传人快照）"
curl -s "$BASE/business/attachment/list?doc_type=RECEIPT_REFUND&doc_id=$RFID" "${AUTH[@]}" \
  | jq -c '{code,total:.data.total,roles:.data.roles,items:[.data.list[]?|{file_name,file_role,file_role_label,uploader_name,file_size}]}'

echo
echo "===== 42) 列表最终态"
curl -s "$BASE/business/erp/receipt-refund/page" "${AUTH[@]}" \
  | jq -c '{code,total:.data.total,list:[.data.list[]|{refund_no,source_receipt_no,customer_name,amount,allocated_amount,status,status_label,method_label,approval_status}]}'

echo
echo "===== 43) 菜单 path 终态（库内，等价于卡面 ③⑤ 的检查 SQL）"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d nearfix2 -c \
  "SELECT m.title, m.path, m.component, m.sort_order, p.title AS parent, m.is_visible
     FROM sys_menu m LEFT JOIN sys_menu p ON p.id = m.parent_id
    WHERE m.deleted_at IS NULL AND m.id IN ('20000000-0000-0000-0000-000000002950'::uuid,
                                            '20000000-0000-0000-0000-000000003071'::uuid);"
echo -n "空 path 页面菜单条数（期望 0）："
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d nearfix2 -tAc \
  "SELECT count(*) FROM sys_menu WHERE deleted_at IS NULL AND menu_type=2 AND (path IS NULL OR path='');"
