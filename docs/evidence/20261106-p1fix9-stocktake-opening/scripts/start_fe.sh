#!/usr/bin/env bash
set -u
cd /opt/cloudpivot/frontend || exit 1
pkill -f "vite.js --port 15190" 2>/dev/null
sleep 1
BACKEND_ORIGIN=http://127.0.0.1:18190 nohup node node_modules/vite/bin/vite.js --port 15190 --host 127.0.0.1 > /root/st9-run/vite.log 2>&1 &
echo "vite pid=$!"
for i in $(seq 1 30); do
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:15190/)
  if [ "$code" = "200" ]; then echo "vite up (health=$code)"; break; fi
  sleep 2
done
curl -s -o /dev/null -w 'index=%{http_code}\n' http://127.0.0.1:15190/
curl -s -o /dev/null -w 'api_proxy=%{http_code}\n' http://127.0.0.1:15190/health
