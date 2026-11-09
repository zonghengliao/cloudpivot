#!/usr/bin/env bash
# HOME-3 接口验收：真实 token 打真接口，输出落到 evidence 目录。
#
# 用法：bash verify-api.sh [base_url]
#   base_url 默认 http://127.0.0.1:18093（隔离实例）
set -u

BASE=${1:-http://127.0.0.1:18093}
OUT=/var/tmp/home3-evidence/api
# 首次登录会被后端要求改密（must_change_password），验收脚本第一次跑完就用新口令；
# 这里默认值即「改密后的口令」，换环境时用 ADMIN_PASSWORD 覆盖。
ADMIN_PASSWORD=${ADMIN_PASSWORD:-Home3@2026Pass}
mkdir -p "$OUT"

echo "== 0. 健康检查（无鉴权）=="
curl -s -m 5 -w '\nHTTP %{http_code}\n' "$BASE/health" | tee "$OUT/00-health.txt"

echo
echo "== 1. 超管登录 =="
LOGIN=$(curl -s -m 10 -X POST "$BASE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASSWORD\"}")
echo "$LOGIN" | tee "$OUT/01-login.json" | head -c 400; echo
TOKEN=$(printf '%s' "$LOGIN" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')
if [ -z "$TOKEN" ]; then echo "!! 登录失败，后续跳过"; exit 1; fi
echo "token 长度: ${#TOKEN}"
echo "$TOKEN" > "$OUT/token.txt"

echo
echo "== 2. 服务器状态（/api/business/monitor/server-status）=="
curl -s -m 10 -w '\nHTTP %{http_code}\n' -H "Authorization: Bearer $TOKEN" \
  "$BASE/api/business/monitor/server-status" | tee "$OUT/02-server-status.txt" | head -c 1200; echo

echo
echo "== 3. 菜单树里是否有「服务器状态」=="
curl -s -m 10 -H "Authorization: Bearer $TOKEN" "$BASE/api/system/auth/menus" \
  | tee "$OUT/03-menus.json" > /dev/null
python3 - "$OUT/03-menus.json" <<'PY'
import json, sys
raw = json.load(open(sys.argv[1]))
def walk(nodes, depth=0):
    for n in nodes:
        if 'server-status' in (n.get('path') or '') or '服务器状态' in (n.get('title') or ''):
            print(json.dumps({k: n.get(k) for k in ('id','title','path','name','menu_type','sort_order','auth_code')}, ensure_ascii=False))
        walk(n.get('children') or [], depth+1)
walk(raw.get('data') or [])
PY

echo
echo "== 4. 诊断包下载（/api/system/server-status/diagnostic）=="
curl -s -m 30 -D "$OUT/04-diagnostic.headers.txt" -o "$OUT/04-diagnostic.zip" \
  -w 'HTTP %{http_code}  size=%{size_download}  type=%{content_type}\n' \
  -H "Authorization: Bearer $TOKEN" "$BASE/api/system/server-status/diagnostic"
grep -iE "^(HTTP|content-type|content-disposition|cache-control)" "$OUT/04-diagnostic.headers.txt"
file "$OUT/04-diagnostic.zip"

echo
echo "== 5. 无 token / 非超管 访问 =="
curl -s -m 5 -o /dev/null -w 'no-token  server-status: HTTP %{http_code}\n' "$BASE/api/business/monitor/server-status"
curl -s -m 5 -o /dev/null -w 'no-token  diagnostic:    HTTP %{http_code}\n' "$BASE/api/system/server-status/diagnostic"
