#!/bin/bash
# LNK-01 真机取证：配一条 STOCK_TRANSFER/PENDING 的 SEND_NOTICE 规则 → 建单提交 → 通知真的发出来
set -u
BASE="http://127.0.0.1:18046"
OUT=/opt/cloudpivot/docs/evidence/20261107-lnk01-stock-transfer/curl_evidence.txt
mkdir -p "$(dirname "$OUT")"
: > "$OUT"
say() { echo -e "\n$*" | tee -a "$OUT"; }

# e2e 可能改过口令，两个都试
TOKEN=""
for PASS in Admin@123456so44 Admin@123456; do
  TOKEN=$(curl -sS -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$PASS\"}" \
    | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
  [ -n "$TOKEN" ] && break
done
say "TOKEN 长度 = ${#TOKEN}"
[ -n "$TOKEN" ] || { echo "登录失败" | tee -a "$OUT"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

say '$ curl -X PUT /api/business/flow/linkage   # 配规则：STOCK_TRANSFER / PENDING → SEND_NOTICE'
curl -sS -X PUT "$BASE/api/business/flow/linkage" -H "$AUTH" -H 'Content-Type: application/json' -d '{
  "name": "LNK01 调拨提交 → 通知制单人",
  "trigger_entity": "STOCK_TRANSFER",
  "trigger_status": "PENDING",
  "action_code": "SEND_NOTICE",
  "action_params": {"mode": "TAKEOVER", "recipient": "CREATOR"},
  "enabled": true,
  "seq": 20,
  "confirm": true
}' -o /tmp/lnk01_rule.json
python3 - <<'PY' | tee -a "$OUT"
import json
d = json.load(open('/tmp/lnk01_rule.json'))['data']
print(json.dumps({'id': d['id'], 'name': d['name'], 'trigger': d['trigger_entity'] + '/' + d['trigger_status'],
                  'action': d['action_code'], 'phase': d['phase'], 'implemented': d['implemented'],
                  'problems': d['problems']}, ensure_ascii=False))
PY

say '$ curl /api/business/flow/linkage?trigger_entity=STOCK_TRANSFER   # 触发点字典里 wired 应为 true'
curl -sS "$BASE/api/business/flow/linkage?trigger_entity=STOCK_TRANSFER" -H "$AUTH" -o /tmp/lnk01_rules.json
python3 - <<'PY' | tee -a "$OUT"
import json
data = json.load(open('/tmp/lnk01_rules.json'))['data']
trigger = next(t for t in data['triggers'] if t['entity'] == 'STOCK_TRANSFER')
print(json.dumps({
    'rules': [{'action': r['action_code'], 'status': r['trigger_status'], 'enabled': r['enabled']} for r in data['rules']],
    'trigger': {'entity': trigger['entity'], 'wired': trigger['wired'], 'wiring': trigger['wiring'][:90] + '…'},
}, ensure_ascii=False, indent=1))
PY

say '$ curl -X POST /api/business/erp/transfer   # 建单（WH01 → WH02，原料A × 2）'
curl -sS -X POST "$BASE/api/business/erp/transfer" -H "$AUTH" -H 'Content-Type: application/json' -d '{
  "from_warehouse_id": "d1370001-0000-4000-8000-000000000001",
  "to_warehouse_id": "d1370001-0000-4000-8000-000000000002",
  "reason": "LNK-01 真机取证",
  "items": [{"material_id": "d1370003-0000-4000-8000-000000000001", "qty": 2}]
}' -o /tmp/lnk01_create.json
TID=$(python3 -c "import json;print(json.load(open('/tmp/lnk01_create.json'))['data']['id'])")
TNO=$(python3 -c "import json;print(json.load(open('/tmp/lnk01_create.json'))['data']['transfer_no'])")
python3 -c "import json;d=json.load(open('/tmp/lnk01_create.json'))['data'];print(json.dumps({'id':d['id'],'transfer_no':d['transfer_no'],'status':d['status'],'total_qty':d['total_qty']},ensure_ascii=False))" | tee -a "$OUT"
say "transfer id=$TID no=$TNO"
[ -n "$TID" ] || exit 1

say '$ curl -X PATCH /api/business/erp/transfer/{id}/status  {"status":"PENDING"}'
curl -sS -X PATCH "$BASE/api/business/erp/transfer/$TID/status" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"status":"PENDING"}' -o /tmp/lnk01_patch.json
python3 -c "import json;d=json.load(open('/tmp/lnk01_patch.json'));print(json.dumps({'code':d['code'],'msg':d['msg'],'status':d.get('data',{}).get('status'),'submitted_at':d.get('data',{}).get('submitted_at')},ensure_ascii=False))" | tee -a "$OUT"

sleep 1
say '# 落库真值：联动通知（幂等键 LNK:STOCK_TRANSFER:PENDING:<id>）'
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so44 -Atc \
  "select notice_no||' | '||title||' | '||target_type||' | '||biz_type||' | '||idempotency_key from sys_notice where idempotency_key='LNK:STOCK_TRANSFER:PENDING:$TID'" \
  | tee -a "$OUT"

say '# 落库真值：状态日志（状态机唯一入口）'
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so44 -Atc \
  "select entity_type||' '||from_status||'→'||to_status from biz_status_log where entity_id='$TID'::uuid and entity_type='STOCK_TRANSFER' order by created_at" \
  | tee -a "$OUT"

say '# 收尾：删掉取证规则（库里恢复 3 条预置）+ 删掉取证单据（DRAFT 可删）'
RULE_ID=$(python3 -c "import json;print(json.load(open('/tmp/lnk01_rule.json'))['data']['id'])")
curl -sS -X DELETE "$BASE/api/business/flow/linkage?id=$RULE_ID&confirm=true" -H "$AUTH" | head -c 200 | tee -a "$OUT"; echo | tee -a "$OUT"
curl -sS -X DELETE "$BASE/api/business/erp/transfer/$TID" -H "$AUTH" | head -c 200 | tee -a "$OUT"; echo | tee -a "$OUT"
