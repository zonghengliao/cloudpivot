#!/usr/bin/env bash
# 把默认存储后端切换为指定 config_code（用于验证「切到 S3 后旧接口仍不受影响」）。
# 用法：bash /root/a2-test-run/flip_default.sh minio-test
set -uo pipefail
BASE=http://127.0.0.1:8025/api
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
TARGET=${1:?需要 config_code}
TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')
AUTH="Authorization: Bearer $TOKEN"
ID=$(curl -s "$BASE/business/storage/config/page?page=1&page_size=50" -H "$AUTH" \
  | jq -r --arg code "$TARGET" '.data.list[]? | select(.config_code==$code) | .id')
[ -z "$ID" ] && { echo "找不到后端 $TARGET"; exit 1; }
curl -s -o /dev/null -w "set default $TARGET -> %{http_code}\n" -X POST "$BASE/business/storage/config/$ID/default" -H "$AUTH"
curl -s "$BASE/business/storage/config/page?page=1&page_size=50" -H "$AUTH" \
  | jq -r '.data.list[] | "  \(.config_code)\tdefault=\(.is_default)\thealth=\(.health_status)"'
