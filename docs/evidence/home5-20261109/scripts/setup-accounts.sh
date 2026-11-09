#!/usr/bin/env bash
# HOME-5 验收：真接口改超管口令 + 建 EMPLOYEE 非超管账号（与 HOME-4 同口径，只换端口/库）。
set -u
BASE=http://127.0.0.1:18097
OUT=/var/tmp/home5/api
DB=cloudpivot_home5_v4
mkdir -p "$OUT"

LOGIN=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}')
TOKEN=$(printf '%s' "$LOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
echo "login must_change_password=$(printf '%s' "$LOGIN" | sed -n 's/.*"must_change_password":\(true\|false\).*/\1/p') token_len=${#TOKEN}"
[ -n "$TOKEN" ] || { echo '!! 登录失败'; exit 1; }

NEW='Home5@2026Pass'
CHG=$(curl -s -m 10 -X PUT "$BASE/api/system/auth/password" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"old_password\":\"Admin@123456\",\"new_password\":\"$NEW\"}")
echo "改密：$CHG"

LOGIN2=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NEW\"}")
TOKEN=$(printf '%s' "$LOGIN2" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
[ -n "$TOKEN" ] || { echo '!! 新口令登录失败'; exit 1; }
echo "$TOKEN" > "$OUT/admin.token"
curl -s -m 10 -H "Authorization: Bearer $TOKEN" "$BASE/api/system/auth/me" > "$OUT/admin-me.json"
python3 - "$OUT/admin-me.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))['data']
print('admin me:', d['username'], 'roles=', d['roles'], 'perms=', len(d['permissions']), 'wildcard=', '*:*:*' in d['permissions'])
PY

ROLE_ID=$(sudo -n -u postgres psql -d "$DB" -t -A -c \
  "SELECT id FROM sys_role WHERE role_code='EMPLOYEE' AND deleted_at IS NULL LIMIT 1")
echo "EMPLOYEE role_id=$ROLE_ID"

CREATE=$(curl -s -m 15 -X POST "$BASE/api/system/user" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"h5viewer\",\"password\":\"Viewer@2026Pass\",\"real_name\":\"HOME-5 验收账号\",\"status\":1,\"role_ids\":[\"$ROLE_ID\"]}")
echo "建账号：$(printf '%s' "$CREATE" | head -c 200)"

VLOGIN=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"h5viewer","password":"Viewer@2026Pass"}')
VTOKEN=$(printf '%s' "$VLOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
echo "viewer token_len=${#VTOKEN}"
[ -n "$VTOKEN" ] && echo "$VTOKEN" > "$OUT/viewer.token"
curl -s -m 10 -H "Authorization: Bearer $VTOKEN" "$BASE/api/system/auth/me" > "$OUT/viewer-me.json"
python3 - "$OUT/viewer-me.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))['data']
print('viewer me:', d['username'], 'roles=', d['roles'], 'perms=', len(d['permissions']), 'wildcard=', '*:*:*' in d['permissions'], 'has_todo_perm=', 'business:flow:instance:read' in d['permissions'])
PY
