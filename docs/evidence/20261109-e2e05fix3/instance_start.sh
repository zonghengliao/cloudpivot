#!/usr/bin/env bash
# 起 E2E05-FIX-3 隔离实例（145 / cloudpivot_dev / 127.0.0.1:8038）
set -euo pipefail
cd /opt/cp-e2e05fix2/backend
set -a
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export NIK_SERVER_LISTEN=127.0.0.1:8038
export NIK_CACHE_TYPE=redis
export NIK_REDIS_URL=redis://127.0.0.1:6379/3
exec ./target/debug/cloudpivot
