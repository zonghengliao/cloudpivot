#!/bin/bash
# CODE-01 145 实测留证（v2）：真实 token 登录 + GET /business/erp/supplier/page
set -u
set -a
. /etc/cloudpivot/cloudpivot-dev.env
set +a
PORT="${NIK_SERVER_LISTEN##*:}"
BASE="http://127.0.0.1:${PORT}"

TOKEN=$(curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"${NIK_ADMIN_PASSWORD}\"}" \
  | grep -o '"access_token":"[^"]*"' | head -1 | cut -d'"' -f4)
[ -n "${TOKEN:-}" ] || { echo "登录失败"; exit 1; }
echo "登录成功：access_token 长度=${#TOKEN}"

echo "== GET /api/business/erp/supplier/page"
curl -s -w '\nHTTP %{http_code}\n' -H "Authorization: Bearer $TOKEN" \
  "$BASE/api/business/erp/supplier/page?page=1&size=3" | head -c 700; echo

echo "== GET /api/system/auth/me（令牌真实生效：换到权限层）"
curl -s -H "Authorization: Bearer $TOKEN" "$BASE/api/system/auth/me" | head -c 300; echo
