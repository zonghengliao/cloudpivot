#!/usr/bin/env bash
# NEAR-FIX-1 真实 token curl 实测（99 · nearfix2 · profile=test · :8099）
# 用法：bash step1.sh   —— 登录 → 改初始口令 → 建客户/应收/收款 → 核销 → 确认 → 过账
set -uo pipefail
BASE=http://127.0.0.1:8099/api
OUT=/root/.hermes/profiles/backend-developer/cache/scratch/nearfix1
R() { jq -r "$1" <<<"$2"; }

login() {
  curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$1\"}"
}

echo "===== 1) 登录（初始口令）"
L=$(login 'Admin@123456')
echo "$L" | jq -c '{code,msg,must_change_password:.data.must_change_password}' 2>/dev/null || echo "$L"
TOKEN=$(R '.data.access_token' "$L")
[ -z "$TOKEN" ] && { echo "登录失败"; exit 1; }

echo "===== 2) 改初始口令 Admin@123456 → Admin@654321"
curl -s -X PUT "$BASE/system/auth/password" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"old_password":"Admin@123456","new_password":"Admin@654321"}' | jq -c '{code,msg}'

echo "===== 3) 用新口令重登"
L=$(login 'Admin@654321')
TOKEN=$(R '.data.access_token' "$L")
echo "token 前缀: ${TOKEN:0:20}…（长度 ${#TOKEN}）"
[ -z "$TOKEN" ] && { echo "重登失败"; exit 1; }
echo "$TOKEN" > "$OUT/token.txt"

AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

echo "===== 4) 建客户"
C=$(curl -s -X POST "$BASE/business/erp/customer" "${AUTH[@]}" \
  -d '{"name":"NEAR-FIX1 退款实测客户","credit_days":0}')
echo "$C" | jq -c '{code,msg,id:.data.id,code_:.data.code}'
CID=$(R '.data.id' "$C")

echo "===== 5) 手工挂账（应收 1000，未收）"
AR=$(curl -s -X POST "$BASE/business/erp/receivable" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"amount\":1000,\"remark\":\"NEAR-FIX1 退款实测\"}")
echo "$AR" | jq -c '{code,msg,id:.data.id,ar_no:.data.ar_no,amount:.data.amount,received:.data.received_amount,status:.data.status}'
ARID=$(R '.data.id' "$AR")

echo "===== 6) 登记收款单（1000，银行回单稍后上传）"
RC=$(curl -s -X POST "$BASE/business/erp/receipt" "${AUTH[@]}" \
  -d "{\"customer_id\":\"$CID\",\"amount\":1000,\"method\":\"BANK_TRANSFER\",\"remark\":\"NEAR-FIX1 收款\"}")
echo "$RC" | jq -c '{code,msg,id:.data.id,no:.data.receipt_no,status:.data.status}'
RCID=$(R '.data.id' "$RC")

echo "===== 7) 收款核销到应收（1000）"
AL=$(curl -s -X POST "$BASE/business/erp/receipt/$RCID/allocate" "${AUTH[@]}" \
  -d "{\"items\":[{\"receivable_id\":\"$ARID\",\"amount\":1000}]}")
echo "$AL" | jq -c '{code,msg,allocated:.data.allocated_amount,unallocated:.data.unallocated_amount}'

echo "===== 8) 确认收款（DRAFT → CONFIRMED；可能被银行回单闸门拦住）"
CF=$(curl -s -X PATCH "$BASE/business/erp/receipt/$RCID/status" "${AUTH[@]}" \
  -d '{"status":"CONFIRMED"}')
echo "$CF" | jq -c '{code,msg,status:.data.status}' 2>/dev/null || echo "$CF"

echo "CID=$CID ARID=$ARID RCID=$RCID" > "$OUT/ids.txt"
cat "$OUT/ids.txt"
