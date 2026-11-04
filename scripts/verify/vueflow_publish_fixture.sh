#!/usr/bin/env bash
# 发布 VUE-FLOW-2 的夹具流程（用真接口，token 从登录回包取）
set -euo pipefail
BASE="${1:-http://127.0.0.1:5188}"
DEF="${2:?需要流程定义 id}"
LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' -d '{"username":"admin","password":"Admin@123456"}')
TOKEN=$(printf '%s' "$LOGIN" | grep -o '"access_token":"[^"]*"' | head -1 | cut -d'"' -f4)
if [ -z "$TOKEN" ]; then
  echo "登录失败: ${LOGIN:0:200}"
  exit 1
fi
printf '%s' "$TOKEN" > /tmp/vf2_token.txt
echo "token 长度: ${#TOKEN}"
echo "--- validate ---"
curl -s -X POST "$BASE/api/business/flow/version/$(curl -s -H "Authorization: Bearer $TOKEN" "$BASE/api/business/flow/def/$DEF" | grep -o '"version_id":"[^"]*"' | head -1 | cut -d'"' -f4)/validate" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{}' | head -c 600
echo
echo "--- publish ---"
curl -s -X POST "$BASE/api/business/flow/def/$DEF/publish" -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{}' | head -c 600
echo
