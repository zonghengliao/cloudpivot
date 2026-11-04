#!/usr/bin/env bash
# 起前端 dev（vite 5183 → 后端 18162），供 UI 真机验收
set -uo pipefail
cd /opt/cloudpivot/frontend || exit 1
export BACKEND_ORIGIN=http://127.0.0.1:18162
setsid nohup pnpm dev --port 5183 --strictPort > /root/ce73ae26-vite.log 2>&1 < /dev/null &
echo "vite pid=$!"
