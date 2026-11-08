#!/bin/bash
# P3-07 后端小补真机验收用的隔离后端：fresh DB cloudpivot_p307be，端口 18313。
# 二进制 = /opt/cloudpivot/backend/target/debug/cloudpivot（本次改动现编）。
# **必须** cd 到 backend/：config/ 与 migrations/ 都是相对 cwd 的，否则数据库根本不初始化。
cd /opt/cloudpivot/backend || exit 1
export NIK_PROFILE=test
export NIK_SERVER_LISTEN=127.0.0.1:18313
export NIK_DATABASE_NAME=cloudpivot_p307be
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
# 密钥（隔离实例专用，取自本机 /etc/cloudpivot/cloudpivot-dev.env 的同名变量；此处不落明文）
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="${NIK_MIDDLEWARE_AUTH_JWT_SECRET:?需 ≥44 字符}"
export CLOUDPIVOT_STORAGE_MASTER_KEY="${CLOUDPIVOT_STORAGE_MASTER_KEY:?需 64 hex}"
export NIK_ADMIN_PASSWORD=Admin@123456
export RUST_LOG=info,cloudpivot=info
exec /opt/cloudpivot/backend/target/debug/cloudpivot
