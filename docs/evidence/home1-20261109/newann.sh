#!/bin/bash
set -euo pipefail
B=http://127.0.0.1:18023
T=$(curl -s -X POST "$B/api/system/auth/login" -H 'Content-Type: application/json' -d '{"username":"admin","password":"Admin@123456"}' | jq -r .data.access_token)
MD=$(printf '## 系统升级公告（V1.2）\n\n本次升级内容：\n\n- 首页新增「公告与消息」卡片\n- 修复若干已知问题\n\n请各位同事留意。')
body=$(jq -nc --arg t "【HOME1】系统升级公告（V1.2）" --arg md "$MD" '{title:$t, content_md:$md, level:"IMPORTANT", scope_type:"ALL"}')
res=$(curl -s -X POST "$B/api/business/announcement" -H "Authorization: Bearer $T" -H 'Content-Type: application/json' -d "$body")
aid=$(echo "$res" | jq -r '.data.id // empty')
echo "create code=$(echo "$res" | jq -r .code) id=$aid"
curl -s -X POST "$B/api/business/announcement/$aid/publish" -H "Authorization: Bearer $T" | jq -c '{code,msg}'
