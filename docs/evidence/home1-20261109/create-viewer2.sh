#!/bin/bash
# 第二个非超管账号：新建后不会收到历史 ALL 范围站内信 → 消息区天然为空态
set -euo pipefail
B=http://127.0.0.1:18023
D=/root/.hermes/profiles/frontend-developer/cache/scratch/home1
T=$(curl -s -X POST "$B/api/system/auth/login" -H 'Content-Type: application/json' -d '{"username":"admin","password":"Admin@123456"}' | jq -r .data.access_token)
ROLE=20000000-0000-4000-8000-0000000f0001
res=$(curl -s -X POST "$B/api/system/user" -H "Authorization: Bearer $T" -H 'Content-Type: application/json' \
  -d "{\"username\":\"h1viewer2\",\"password\":\"Viewer@123456\",\"real_name\":\"首页验收员二号\",\"status\":1,\"role_ids\":[\"$ROLE\"]}")
echo "create: $(echo "$res" | jq -c '{code,msg}')"
curl -s -X POST "$B/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"h1viewer2","password":"Viewer@123456"}' > "$D/login-viewer2.json"
VT=$(jq -r .data.access_token "$D/login-viewer2.json")
echo "login: $(jq -c '{code,msg,has:(.data.access_token!=null)}' "$D/login-viewer2.json")"
echo "notices: $(curl -s "$B/api/business/notice/mine?page=1&page_size=10" -H "Authorization: Bearer $VT" | jq -c '{code,total:.data.total,rows:(.data.list|length)}')"
jq -nc --arg at "$VT" --arg rt "$(jq -r .data.refresh_token "$D/login-viewer2.json")" '{at:$at,rt:$rt}' > "$D/tokens-viewer2.json"
