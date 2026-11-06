#!/usr/bin/env bash
# SO-18 隔离实例启动（前端卡手工验收用）
#   DB: cloudpivot_so18 @ 127.0.0.1:5432（alun/alun）
#   端口: 8023（前端 dev 代理目标）
set -euo pipefail
export NIK_DATABASE_NAME=cloudpivot_so18
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN=127.0.0.1:8023
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export X_CONFIG_SECRET_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD='Admin@123456'
exec ./target/debug/cloudpivot
