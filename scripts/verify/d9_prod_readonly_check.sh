#!/usr/bin/env bash
# D9 生产只读核对（144）：不建任何单据，只证明「三条新路由已在线上注册」
# 判据：新路由无令牌 → 401（路由匹配到了，被认证层拦）；不存在的路由 → 404
set -uo pipefail
B=http://127.0.0.1:8023
Z=00000000-0000-0000-0000-000000000000

code() { curl -s -o /tmp/d9_prod.out -w '%{http_code}' -m 5 -X POST "$1" -H 'Content-Type: application/json' -d '{}'; }

echo "== 新路由（无令牌应 401，不是 404）=="
for path in \
  "/api/business/erp/stock-in/$Z/copy" \
  "/api/business/erp/transfer/$Z/copy" \
  "/api/business/task/$Z/copy"; do
  c=$(code "$B$path")
  echo "  POST $path → $c $(head -c 80 /tmp/d9_prod.out)"
done

echo "== 对照：不存在的路由应 404 =="
c=$(code "$B/api/business/erp/definitely-not-a-route/xyz")
echo "  POST /api/business/erp/definitely-not-a-route/xyz → $c"

echo "== 对照：既有建单路由（无令牌也 401，证明判据有效）=="
c=$(code "$B/api/business/erp/stock-in")
echo "  POST /api/business/erp/stock-in → $c"

echo "== 前端 dist 入口 chunk 与新 testid =="
grep -oh "BasicLayout-[A-Za-z0-9_-]*\.js" /opt/cloudpivot/frontend/dist/index.html /opt/cloudpivot/frontend/dist/assets/index-*.js 2>/dev/null | head -1
for token in btn-stockin-copy stockin-copy-notice btn-transfer-copy transfer-copy-notice btn-task-copy; do
  n=$(grep -rl "$token" /opt/cloudpivot/frontend/dist/assets/*.js 2>/dev/null | wc -l)
  echo "  $token → $n"
done

echo "== 库侧：三处权限码仍在（零迁移的前提）=="
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h "$NIK_DATABASE_HOST" -p "$NIK_DATABASE_PORT" \
  -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -tAc \
  "select count(*) from sys_menu where auth_code in ('business:erp:stock-in:create','business:erp:transfer:create','business:task:create')"
