#!/usr/bin/env bash
# M1-20 真接口验收：起一个 Debug 实例（:8023，库 cp_m120_api），供 api_check.py 打。
set -euo pipefail

cd /opt/cloudpivot/backend

export NIK_PROFILE=test
export NIK_DATABASE_HOST=172.18.10.145
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_NAME=cp_m120_api2
export NIK_DATABASE_USER=cloudpivot
export NIK_DATABASE_PASSWORD=fbb1263691ff75c31ec99eaf258138b4
export NIK_ADMIN_PASSWORD='Admin@123456'
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export X_CONFIG_SECRET_KEY="$(openssl rand -hex 16)"

exec ./target/debug/cloudpivot
