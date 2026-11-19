#!/usr/bin/env bash
# M5-31 隔离实例（证据用）：连种子完备库，起在 18076，供 m531_http_smoke.py 打真实 HTTP。
#
# 口令与密钥一律现场取（环境变量 / openssl），仓里不落任何密钥：
#   PGPASSWORD=<演练库口令> bash m531-start.sh
#
# 空库实例默认 erp.cost.enabled = false（成本接口 403 + 66079），冒烟前先打开再重启：
#   psql -h $M531_PGHOST -U cloudpivot -d $M531_DB \
#     -c "update sys_config set config_value='true' where config_key='erp.cost.enabled'"
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
: "${PGPASSWORD:?需要 PGPASSWORD（演练库口令；99 上 PG 在 172.18.10.145）}"
REPO=${REPO:-/opt/cloudpivot}
export NIK_PROFILE=test
export NIK_DATABASE_NAME=${M531_DB:-cloudpivot_test_m531}
export NIK_DATABASE_HOST=${M531_PGHOST:-172.18.10.145}
export NIK_DATABASE_PORT=${M531_PGPORT:-5432}
export NIK_DATABASE_USER=${M531_PGUSER:-cloudpivot}
export NIK_DATABASE_PASSWORD="$PGPASSWORD"
export NIK_SERVER_LISTEN=${M531_LISTEN:-127.0.0.1:18076}
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export X_CONFIG_SECRET_KEY="$(openssl rand -hex 16)"
export NIK_ADMIN_PASSWORD=${M531_ADMIN_PW:-'Admin@123456'}
LOG=${M531_LOG:-${TMPDIR:-/tmp}/m531-http.log}
PIDFILE=${M531_PIDFILE:-${TMPDIR:-/tmp}/m531-http.pid}
: > "$LOG"
cd "$REPO/backend" || exit 1
setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"
echo "started pid=$(cat "$PIDFILE") log=$LOG"
