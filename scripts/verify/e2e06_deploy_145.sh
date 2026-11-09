#!/usr/bin/env bash
# 145 部署：pull + build + restart + health（供 E2E-06 使用；不碰 144）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot || exit 1
git pull -q origin main || { echo "PULL_FAIL"; exit 1; }
git log --oneline -1
cargo build --bin cloudpivot --manifest-path backend/Cargo.toml > /tmp/e2e06_build.log 2>&1
rc=$?
tail -3 /tmp/e2e06_build.log
if [ $rc -ne 0 ]; then echo "BUILD_FAIL"; exit 1; fi
systemctl restart cloudpivot-test
for i in $(seq 1 30); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 3 http://127.0.0.1:8033/health)
  [ "$code" = "200" ] && { echo "HEALTH_OK after ${i}s"; exit 0; }
  sleep 1
done
echo "HEALTH_FAIL"; exit 1
