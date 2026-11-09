#!/usr/bin/env bash
# HOME-3 权限验收：非超管看不到菜单、直连接口 403。
#
# 步骤：
#   1) 超管建一个只有 EMPLOYEE 角色的账号 h3viewer（真实走 POST /api/system/user）；
#   2) h3viewer 登录，拉菜单树 → 「服务器状态」必须**不出现**；
#   3) h3viewer 直连页面数据接口 / 诊断包接口 → 必须 403。
set -u

BASE=${1:-http://127.0.0.1:18093}
OUT=/var/tmp/home3-evidence/api
ADMIN_PASSWORD=${ADMIN_PASSWORD:-Home3@2026Pass}
VIEWER_PASSWORD='Viewer@2026Pass'
mkdir -p "$OUT"

ADMIN_TOKEN=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASSWORD\"}" \
  | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
[ -n "$ADMIN_TOKEN" ] || { echo "!! 超管登录失败"; exit 1; }

ROLE_ID=$(cd /var/tmp/home3-evidence && sudo -u postgres psql -d cloudpivot_home3_v1 -t -A -c \
  "SELECT id FROM sys_role WHERE role_code='EMPLOYEE' AND deleted_at IS NULL LIMIT 1")
echo "EMPLOYEE role_id=$ROLE_ID"

echo
echo "== 创建非超管账号 h3viewer（EMPLOYEE）=="
curl -s -m 15 -X POST "$BASE/api/system/user" -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"h3viewer\",\"password\":\"$VIEWER_PASSWORD\",\"real_name\":\"HOME-3 验收账号\",\"status\":1,\"role_ids\":[\"$ROLE_ID\"]}" \
  -w '\nHTTP %{http_code}\n' | tee "$OUT/10-create-viewer.txt" | head -5

echo
echo "== h3viewer 登录 =="
VLOGIN=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"h3viewer\",\"password\":\"$VIEWER_PASSWORD\"}")
echo "$VLOGIN" | head -c 300; echo
VTOKEN=$(printf '%s' "$VLOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
echo "$VTOKEN" > "$OUT/viewer-token.txt"
[ -n "$VTOKEN" ] || { echo "!! 非超管登录失败（可能仍需改密）"; exit 1; }

echo
echo "== 非超管：菜单树是否包含「服务器状态」=="
curl -s -m 10 -H "Authorization: Bearer $VTOKEN" "$BASE/api/system/auth/menus" > "$OUT/11-viewer-menus.json"
PUT=$(python3 - "$OUT/11-viewer-menus.json" <<'PY'
import json, sys
raw = json.load(open(sys.argv[1]))
hits = []
def walk(nodes):
    for n in nodes:
        if 'server-status' in (n.get('path') or '') or '服务器状态' in (n.get('title') or ''):
            hits.append(n.get('path'))
        walk(n.get('children') or [])
walk(raw.get('data') or [])
print(len(hits))
PY
)
echo "菜单里 server-status 命中数: $PUT（期望 0）"

echo
echo "== 非超管：直连接口 =="
curl -s -m 10 -w '\nHTTP %{http_code}\n' -H "Authorization: Bearer $VTOKEN" \
  "$BASE/api/business/monitor/server-status" | tee "$OUT/12-viewer-server-status.txt" | head -4
curl -s -m 10 -w '\nHTTP %{http_code}\n' -H "Authorization: Bearer $VTOKEN" \
  "$BASE/api/system/server-status/diagnostic" | tee "$OUT/13-viewer-diagnostic.txt" | head -4

echo
echo "== 超管：诊断包下载后在 operation_logs 里留痕 =="
cd /var/tmp/home3-evidence && sudo -u postgres psql -d cloudpivot_home3_v1 -c \
  "SELECT username, operation, target_type, target_id, status_code FROM operation_logs WHERE operation='SYSTEM_SERVER_STATUS_DIAGNOSTIC' ORDER BY created_at DESC LIMIT 3;" \
  | tee "$OUT/14-operation-logs.txt"
