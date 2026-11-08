#!/bin/bash
# P3-07 前端配套验收：隔离环境一键准备（DB 已由调用方 drop/create + 重启后端）
#
# 做完这些就停在「开关打开、三张应付单就绪、三个令牌就绪」的状态，
# 之后跑 `node ../verify/p307e2e.mjs` 走真机用例。
set -uo pipefail
cd "$(dirname "$0")"

API=http://127.0.0.1:18312/api
DB=cloudpivot_p307fe
PG="psql -h 127.0.0.1 -U alun -d $DB"
export PGPASSWORD=alun

echo "== 等后端健康 =="
for i in $(seq 1 90); do
  if curl -sf -m 2 http://127.0.0.1:18312/health > /dev/null; then echo "ok ($i s)"; break; fi
  sleep 1
done

echo "== admin 首登改密 =="
curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' -o login.json
T=$(jq -r .data.access_token login.json)
curl -s -X PUT "$API/system/auth/password" -H "Authorization: Bearer $T" -H 'Content-Type: application/json' \
  -d '{"old_password":"Admin@123456","new_password":"P307@Veri2026"}' > /dev/null
curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"P307@Veri2026"}' -o login2.json
jq -r .data.access_token login2.json > admin.token
echo "admin token len=$(wc -c < admin.token)"

echo "== 建两个账号（发起付款人 / 部门主管=审批人） =="
curl -s -X POST "$API/system/user" -H "Authorization: Bearer $(cat admin.token)" -H 'Content-Type: application/json' \
  -d '{"username":"p307.pay","password":"P307@Veri2026","real_name":"付款发起人","status":1,"role_ids":["20000000-0000-4000-8000-0000000f0005"]}' \
  | jq -c '{code,msg}'
curl -s -X POST "$API/system/user" -H "Authorization: Bearer $(cat admin.token)" -H 'Content-Type: application/json' \
  -d '{"username":"p307.mgr","password":"P307@Veri2026","real_name":"部门主管","status":1,"role_ids":["20000000-0000-4000-8000-0000000f0001","20000000-0000-4000-8000-0000000f0005"]}' \
  | jq -c '{code,msg}'

for u in pay mgr; do
  curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"p307.$u\",\"password\":\"P307@Veri2026\"}" -o "u_$u.json"
  jq -r .data.access_token "u_$u.json" > "$u.token"
  echo "p307.$u token len=$(wc -c < $u.token) must_change=$(jq -r .data.must_change_password u_$u.json)"
done

echo "== 打开 erp.separation_of_duties.approve_execute（系统参数 API） =="
CID=e2b00000-0000-4000-8000-000000000006
curl -s "$API/support/config/$CID" -H "Authorization: Bearer $(cat admin.token)" -o cfg.json
jq -c '.data | {config_key, config_value:"true", config_type, description, category, sort_order, config_level, is_modifiable, is_encrypted, is_restart, status}' cfg.json > cfgput.json
curl -s -X PUT "$API/support/config/$CID" -H "Authorization: Bearer $(cat admin.token)" -H 'Content-Type: application/json' \
  -d @cfgput.json | jq -c '{code, value:.data.config_value}'

echo "== 应付单夹具（含供应商） =="
$PG -v ON_ERROR_STOP=1 -q -f fixture.sql
$PG -v ON_ERROR_STOP=1 -q -f fixture2.sql

echo "== 就绪 =="
$PG -At -F'|' -c "select payable_no, amount, paid_amount, status from biz_payable where payable_no like 'AP-P307-%' order by 1"
