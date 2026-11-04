#!/usr/bin/env bash
# 起 t_ce73ae26 的隔离实例（库 cloudpivot_ce73ae26 / 端口 18162），用于 HTTP 验收。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
export NIK_PROFILE=test
export NIK_DATABASE_NAME=cloudpivot_ce73ae26
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN=127.0.0.1:18162
export NIK_MIDDLEWARE_AUTH_JWT_SECRET=ce73ae26ce73ae26ce73ae26ce73ae26ce73ae26ce73ae26ce73ae26ce73ae26
export CLOUDPIVOT_STORAGE_MASTER_KEY=ce73ae26ce73ae26ce73ae26ce73ae26ce73ae26ce73ae26ce73ae26ce73ae26
export NIK_ADMIN_PASSWORD=Admin@123456
LOG=/root/ce73ae26-instance.log
PIDFILE=/root/ce73ae26-instance.pid
: > "$LOG"
cd /opt/cloudpivot/backend || exit 1
setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"
echo "started pid=$(cat "$PIDFILE")"
