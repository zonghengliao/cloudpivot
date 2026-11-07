#!/bin/bash
# SO-43 原始 curl 取证（真实 token + 真实 HTTP，输出逐条落证据文件）
set -u
BASE=http://127.0.0.1:18043
CUST=aa430000-0000-4000-8000-000000000001
OUT=/opt/cloudpivot/docs/evidence/20261107-so43-statement/curl_evidence.txt
: > "$OUT"

say() { echo -e "\n$*" | tee -a "$OUT"; }

TOKEN=$(curl -sS -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456so43"}' \
  | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
say '$ TOKEN=$(curl -sS -X POST $BASE/api/system/auth/login -d ...)   # 长度 '${#TOKEN}
[ -n "$TOKEN" ] || { echo "登录失败" | tee -a "$OUT"; exit 1; }

say '$ curl -sS "$BASE/api/business/erp/receivable/statement?customer_id=$CUST&period_from=2026-10-01&period_to=2026-10-31" -H "Authorization: Bearer $TOKEN"'
curl -sS "$BASE/api/business/erp/receivable/statement?customer_id=$CUST&period_from=2026-10-01&period_to=2026-10-31" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool | tee -a "$OUT"

say '$ curl -sS -D- -o /tmp/so43-statement.xlsx "...statement/export?..." -H "Authorization: Bearer $TOKEN"'
curl -sS -D- -o /tmp/so43-statement.xlsx \
  "$BASE/api/business/erp/receivable/statement/export?customer_id=$CUST&period_from=2026-10-01&period_to=2026-10-31" \
  -H "Authorization: Bearer $TOKEN" | tee -a "$OUT"
say '$ file /tmp/so43-statement.xlsx && ls -l /tmp/so43-statement.xlsx'
file /tmp/so43-statement.xlsx | tee -a "$OUT"
ls -l /tmp/so43-statement.xlsx | tee -a "$OUT"
say '$ unzip -p /tmp/so43-statement.xlsx xl/sharedStrings.xml | head -c 600'
unzip -p /tmp/so43-statement.xlsx xl/sharedStrings.xml | head -c 600 | tee -a "$OUT"
echo | tee -a "$OUT"

say '$ curl -sS -X POST "$BASE/api/business/erp/receivable/statement/confirm" -d {customer,period,...} -H "Authorization: Bearer $TOKEN"'
curl -sS -X POST "$BASE/api/business/erp/receivable/statement/confirm" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"customer_id\":\"$CUST\",\"period_from\":\"2026-10-01\",\"period_to\":\"2026-10-31\",\"remark\":\"curl 取证\"}" \
  | python3 -m json.tool | tee -a "$OUT"

say '# 无 token（401）'
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' \
  "$BASE/api/business/erp/receivable/statement?customer_id=$CUST&period_from=2026-10-01" | tee -a "$OUT"

echo "证据已写入 $OUT" 
