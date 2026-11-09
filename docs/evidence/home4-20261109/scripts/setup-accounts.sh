#!/usr/bin/env bash
# HOME-4 验收的账号准备：真接口改超管口令 + 建一个 EMPLOYEE 非超管账号。
#
# 用法：bash /var/tmp/home4/setup-accounts.sh
set -u
BASE=http://127.0.0.1:18094
OUT=/var/tmp/home4/api
mkdir -p "$OUT"

# 1) 超管登录（空库首启的种子口令）
LOGIN=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}')
echo "$LOGIN" | head -c 300; echo
TOKEN=$(printf '%s' "$LOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
MUST=$(printf '%s' "$LOGIN" | sed -n 's/.*"must_change_password":\(true\|false\).*/\1/p')
echo "token len=${#TOKEN} must_change_password=$MUST"
[ -n "$TOKEN" ] || { echo '!! 登录失败'; exit 1; }

# 2) 首登必须改密：改成 Home4@2026Pass
NEW='Home4@2026Pass'
CHG=$(curl -s -m 10 -X PUT "$BASE/api/system/auth/password" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"old_password\":\"Admin@123456\",\"new_password\":\"$NEW\"}")
echo "改密：$CHG" | head -c 200; echo

# 3) 用新口令登录，拿干净 token
LOGIN2=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$NEW\"}")
echo "$LOGIN2" | head -c 200; echo
TOKEN=$(printf '%s' "$LOGIN2" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
[ -n "$TOKEN" ] || { echo '!! 新口令登录失败'; exit 1; }
echo "$TOKEN" > "$OUT/admin.token"

# 4) /me 确认角色与权限
curl -s -m 10 -H "Authorization: Bearer $TOKEN" "$BASE/api/system/auth/me" > "$OUT/admin-me.json"
python3 - "$OUT/admin-me.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))['data']
print('me:', d['username'], 'roles=', d['roles'], 'perms=', len(d['permissions']), 'super_wildcard=', '*:*:*' in d['permissions'])
PY

# 5) 取 EMPLOYEE 角色 id
ROLE_ID=$(sudo -u postgres psql -d cloudpivot_home4_v1 -t -A -c \
  "SELECT id FROM sys_role WHERE role_code='EMPLOYEE' AND deleted_at IS NULL LIMIT 1")
echo "EMPLOYEE role_id=$ROLE_ID"

# 6) 建非超管账号 h4viewer（真实走 POST /api/system/user）
CREATE=$(curl -s -m 15 -X POST "$BASE/api/system/user" -H "Authorization: Bearer $TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"h4viewer\",\"password\":\"Viewer@2026Pass\",\"real_name\":\"HOME-4 验收账号\",\"status\":1,\"role_ids\":[\"$ROLE_ID\"]}")
echo "建账号：$(echo "$CREATE" | head -c 200)"

# 7) h4viewer 登录 + /me（确认 roles 不含 SUPER_ADMIN）
VLOGIN=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"h4viewer","password":"Viewer@2026Pass"}')
echo "$VLOGIN" | head -c 200; echo
VTOKEN=$(printf '%s' "$VLOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
[ -n "$VTOKEN" ] && echo "$VTOKEN" > "$OUT/viewer.token"
curl -s -m 10 -H "Authorization: Bearer $VTOKEN" "$BASE/api/system/auth/me" > "$OUT/viewer-me.json"
python3 - "$OUT/viewer-me.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))['data']
print('viewer me:', d['username'], 'roles=', d['roles'], 'perms=', len(d['permissions']), 'wildcard=', '*:*:*' in d['permissions'])
PY
