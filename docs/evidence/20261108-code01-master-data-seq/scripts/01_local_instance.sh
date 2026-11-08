#!/bin/bash
# CODE-01 隔离实例（99 本机）：profile=test + 独立库 + 18337 端口
set -u
set -a
export NIK_PROFILE=test
export NIK_DATABASE_NAME=cloudpivot_code01
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN=127.0.0.1:18337
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="code01-jwt-secret-0123456789abcdef0123456789abcdef"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export X_CONFIG_SECRET_KEY="code01-config-secret"
set +a

cd /root/code01-src2/backend || exit 1
setsid nohup /root/code01-target/debug/cloudpivot > /root/code01-instance.log 2>&1 < /dev/null &
echo $! > /root/code01-instance.pid
echo "started pid=$(cat /root/code01-instance.pid)"
