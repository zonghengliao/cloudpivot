#!/usr/bin/env bash
# ⑤ 附件闸门真机验证：造一张 DRAFT 盘点单（无附件）→ check API → PATCH 兜底
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="http://127.0.0.1:8033/api"
OUT=/tmp/e2efix8; mkdir -p "$OUT"
PSQLQ(){ psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
        -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')

WH=$(PSQLQ -c "SELECT w.id FROM biz_warehouse w WHERE w.deleted_at IS NULL AND w.status = 1 AND EXISTS (SELECT 1 FROM biz_inventory i WHERE i.warehouse_id = w.id) ORDER BY w.warehouse_code LIMIT 1")
echo "仓库 = $WH"
echo "== 建 DRAFT 盘点单（全盘）"
curl -s -X POST "$BASE/business/erp/stock-take" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"warehouse_id\":\"$WH\",\"take_type\":\"FULL\",\"remark\":\"E2E-FIX-8 ⑤ 附件闸门验证（可删）\"}" \
  -o "$OUT/take_create.json" -w 'HTTP %{http_code}\n'
jq -c '{code, msg, id: .data.id, take_no: .data.take_no, status: .data.status}' "$OUT/take_create.json"
TAKE_ID=$(jq -r '.data.id // empty' "$OUT/take_create.json")
[ -z "$TAKE_ID" ] && { echo "建单失败"; exit 1; }

echo "== 附件数 / 闸门 check"
PSQLQ -c "SELECT count(*) FROM biz_doc_attachment WHERE doc_type='STOCK_TAKE' AND doc_id='$TAKE_ID'::uuid AND deleted_at IS NULL"
curl -s "$BASE/business/attachment/check?doc_type=STOCK_TAKE&doc_id=$TAKE_ID&at=CONFIRM" \
  -H "Authorization: Bearer $TOKEN" -o "$OUT/att_check2.json" -w 'check HTTP %{http_code}\n'
jq -c '{code, can_proceed: .data.can_proceed, at_label: .data.at_label, hint: .data.hint, missing: [.data.missing[]? | {role_label, need, have, min}]}' "$OUT/att_check2.json"

echo "== 直接 PATCH status=PENDING_POST（后端兜底闸门）"
curl -s -X PATCH "$BASE/business/erp/stock-take/$TAKE_ID/status" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' -d '{"status":"PENDING_POST"}' -o "$OUT/att_patch2.json" -w 'HTTP %{http_code}\n'
jq -c '{code, msg}' "$OUT/att_patch2.json"

echo "== 清理：删掉这张验证单（DRAFT 可删，不动任何账）"
curl -s -X DELETE "$BASE/business/erp/stock-take/$TAKE_ID" -H "Authorization: Bearer $TOKEN" -o "$OUT/take_del.json" -w 'HTTP %{http_code}\n'
jq -c '{code, msg}' "$OUT/take_del.json"
echo "TAKE_ID=$TAKE_ID"
