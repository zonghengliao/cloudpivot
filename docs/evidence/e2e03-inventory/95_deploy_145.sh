#!/usr/bin/env bash
# 145 部署：cargo build（debug 增量）→ 重启 cloudpivot-test → health
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend || exit 1
echo "== build start $(date +%T)"
CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=3 cargo build --bin cloudpivot > /tmp/e2e03_build.log 2>&1
RC=$?
echo "build_rc=$RC $(date +%T)"
tail -5 /tmp/e2e03_build.log
[ "$RC" = 0 ] || exit 1
systemctl restart cloudpivot-test
for i in $(seq 1 20); do
  sleep 2
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8033/health)
  if [ "$code" = 200 ]; then echo "health=200 after ${i}x2s $(date +%T)"; break; fi
done
systemctl is-active cloudpivot-test
