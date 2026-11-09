#!/usr/bin/env bash
# 145 重新构建 + 重启 cloudpivot-test（E2E-05 演练用）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend || exit 1
echo "== build start $(date +%T)"
timeout 1800 cargo build 2>&1 | tail -5
echo "== restart"
systemctl restart cloudpivot-test
for i in $(seq 1 30); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 3 http://127.0.0.1:8033/health)
  [ "$code" = "200" ] && { echo "== health=$code at $(date +%T)"; exit 0; }
  sleep 1
done
echo "== health FAILED"
systemctl status cloudpivot-test --no-pager | tail -20
exit 1
