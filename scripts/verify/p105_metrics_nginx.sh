#!/usr/bin/env bash
# P1-05 验收：用 frontend/deploy/nginx.conf 里的 `/metrics` 片段起一个临时 nginx（:18080），
# 验证「nginx 白名单 + 清空来源头 → 后端按回环放行」这条链路。
#
# 前置：后端在 127.0.0.1:8023 跑着（`GET /metrics` 可用）。
# 用法：bash scripts/verify/p105_metrics_nginx.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PORT="${1:-18080}"
BACKEND_PORT="${2:-8023}"
DIR=/tmp/p105-nginx
CONF="$DIR/nginx.conf"
SITE="$DIR/site.conf"

mkdir -p "$DIR"
# 从仓库模板复制，只改监听端口与 root（其余 location 原样保留 —— 要验的就是模板本身）
sed -e "s/listen       80;/listen       ${PORT};/" \
    -e "s/listen       \[::\]:80;/listen       [::]:${PORT};/" \
    -e "s#root  /opt/cloudpivot/frontend/dist;#root  $DIR;#" \
    -e "s/server 127.0.0.1:8023;/server 127.0.0.1:${BACKEND_PORT};/" \
    "$ROOT/frontend/deploy/nginx.conf" > "$SITE"

cat > "$CONF" <<EOF
worker_processes 1;
error_log $DIR/error.log warn;
pid $DIR/nginx.pid;
events { worker_connections 64; }
http {
  access_log $DIR/access.log;
  client_body_temp_path $DIR/client_body;
  proxy_temp_path $DIR/proxy;
  fastcgi_temp_path $DIR/fastcgi;
  uwsgi_temp_path $DIR/uwsgi;
  scgi_temp_path $DIR/scgi;
  include $SITE;
}
EOF

nginx -t -c "$CONF"
nginx -c "$CONF"
trap 'nginx -c "$CONF" -s stop 2>/dev/null || true' EXIT
sleep 1

LAN_IP=$(hostname -I | awk '{print $1}')
echo "== ① 回环访问（命中 allow 127.0.0.1） =="
curl -s -o "$DIR/loop.txt" -w "http=%{http_code} bytes=%{size_download}\n" "http://127.0.0.1:${PORT}/metrics"
printf 'cloudpivot_* 序列数=%s\n' "$(grep -c '^cloudpivot_' "$DIR/loop.txt" || true)"

echo "== ② 非白名单来源（$LAN_IP，应 403） =="
curl -s -o /dev/null -w "http=%{http_code}\n" "http://${LAN_IP}:${PORT}/metrics"

echo "== ③ 客户端伪造 X-Forwarded-For: 127.0.0.1（nginx 已清空该头 → 后端仍按回环放行） =="
curl -s -o /dev/null -w "http=%{http_code}\n" -H 'X-Forwarded-For: 127.0.0.1' "http://127.0.0.1:${PORT}/metrics"

echo "== ④ 对照：绕过 nginx 直连后端并伪造外部来源（后端自身应 403） =="
curl -s -o /dev/null -w "http=%{http_code}\n" -H 'X-Forwarded-For: 203.0.113.9' "http://127.0.0.1:${BACKEND_PORT}/metrics"
