#!/usr/bin/env bash
# 145 部署：pull + cargo build --bin cloudpivot + 重启 cloudpivot-test
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot || exit 1
git fetch -q origin && git merge --ff-only -q origin/main || { echo "MERGE_FAIL"; git status --porcelain | head; exit 1; }
git log --oneline -1
cd backend || exit 1
CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=3 cargo build --bin cloudpivot 2>&1 | tail -5
RC=${PIPESTATUS[0]}
echo "build_rc=$RC"
[ "$RC" = 0 ] || exit 1
systemctl restart cloudpivot-test
sleep 4
curl -s -o /dev/null -w 'health=%{http_code}\n' http://127.0.0.1:8033/health
