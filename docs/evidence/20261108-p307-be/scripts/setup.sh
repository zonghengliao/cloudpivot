#!/bin/bash
# P3-07 后端小补真机验收：隔离环境准备（库 cloudpivot_p307be 由调用方先 drop/create + 后端 :18313 已起）
# 做完停在「开关打开 / 三个账号 / 两张应付单 / 三个令牌」就绪的状态，之后跑 node verify.mjs。
set -uo pipefail
cd "$(dirname "$0")"

API=http://127.0.0.1:18313/api
DB=cloudpivot_p307be
PG="psql -h 127.0.0.1 -U alun -d $DB"
export PGPASSWORD=alun

ROLE_FINANCE=20000000-0000-4000-8000-0000000f0005   # FINANCE_MANAGER（含 payable:pay / payable:detail）

echo "== 等后端健康 =="
for i in $(seq 1 120); do
  if curl -sf -m 2 http://127.0.0.1:18313/health > /dev/null; then echo "ok ($i s)"; break; fi
  sleep 1
done
curl -sf -m 2 http://127.0.0.1:18313/health || { echo "FATAL: 后端没起来"; exit 1; }

echo "== admin 首登 + 改密 =="
curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' -o login.json
T=$(jq -r .data.access_token login.json)
curl -s -X PUT "$API/system/auth/password" -H "Authorization: Bearer $T" -H 'Content-Type: application/json' \
  -d '{"old_password":"Admin@123456","new_password":"P307BE@Veri2026"}' > /dev/null
curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"P307BE@Veri2026"}' -o login2.json
jq -r .data.access_token login2.json > admin.token
echo "admin token len=$(wc -c < admin.token)"

echo "== 建三个账号：付款发起人 / 执行人（非审批参与人，同权限） =="
for u in be.pay be.exec; do
  curl -s -X POST "$API/system/user" -H "Authorization: Bearer $(cat admin.token)" -H 'Content-Type: application/json' \
    -d "{\"username\":\"p307$u\",\"password\":\"P307BE@Veri2026\",\"real_name\":\"P307BE $u\",\"status\":1,\"role_ids\":[\"$ROLE_FINANCE\"]}" \
    | jq -c '{code,msg}'
done
for u in pay exec; do
  curl -s -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"p307be.$u\",\"password\":\"P307BE@Veri2026\"}" -o "u_$u.json"
  jq -r .data.access_token "u_$u.json" > "$u.token"
  echo "p307be.$u token len=$(wc -c < $u.token) must_change=$(jq -r .data.must_change_password u_$u.json)"
done

echo "== 打开 erp.separation_of_duties.approve_execute（系统参数 API） =="
CID=e2b00000-0000-4000-8000-000000000006
curl -s "$API/support/config/$CID" -H "Authorization: Bearer $(cat admin.token)" -o cfg.json
jq -c '.data | {config_key, config_value:"true", config_type, description, category, sort_order, config_level, is_modifiable, is_encrypted, is_restart, status}' cfg.json > cfgput.json
curl -s -X PUT "$API/support/config/$CID" -H "Authorization: Bearer $(cat admin.token)" -H 'Content-Type: application/json' \
  -d @cfgput.json | jq -c '{code, value:.data.config_value}'

echo "== 应付单夹具 =="
$PG -v ON_ERROR_STOP=1 -q -f fixture.sql

echo "== 就绪 =="
$PG -At -F'|' -c "select payable_no, amount, status from biz_payable where payable_no like 'AP-BE1-%' order by 1"
