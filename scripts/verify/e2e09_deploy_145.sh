#!/usr/bin/env bash
# E2E-09 部署到 145：ff-merge origin/main → 重建 backend → 重启 cloudpivot-test → 等 health=200
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot
echo "== 部署前 HEAD: $(git rev-parse --short HEAD)"
git fetch -q origin
git merge --ff-only origin/main 2>&1 | tail -3
echo "== 部署后 HEAD: $(git rev-parse --short HEAD)"
cargo build --manifest-path backend/Cargo.toml --bin cloudpivot 2>&1 | tail -3
systemctl restart cloudpivot-test
for i in $(seq 1 40); do
  sleep 2
  code=$(curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8033/health || true)
  [ "$code" = "200" ] && { echo "health=200 (after ${i}x2s)"; break; }
done
systemctl is-active cloudpivot-test
