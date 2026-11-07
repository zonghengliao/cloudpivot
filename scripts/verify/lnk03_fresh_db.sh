#!/bin/bash
# 干净验收库 cloudpivot_lnk03：起一次实例只为跑迁移（18050），迁移完即可停
set -u
cd /opt/cloudpivot/backend
export PATH=/root/.cargo/bin:$PATH
mkdir -p /root/tmp-cargo
exec env \
  NIK_PROFILE=test \
  NIK_SERVER_LISTEN=127.0.0.1:18050 \
  NIK_DATABASE_HOST=127.0.0.1 \
  NIK_DATABASE_NAME=cloudpivot_lnk03 \
  NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun \
  NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET='lnk03-fresh-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
  CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
  X_CONFIG_SECRET_KEY='lnk03-fresh-config-secret-0123456789' \
  ./target/debug/cloudpivot
