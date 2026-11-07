#!/bin/bash
# LNK-03 隔离验收入口：起真机实例（DB=cloudpivot_so44，端口 18049）
set -u
cd /opt/cloudpivot/backend
export PATH=/root/.cargo/bin:$PATH
mkdir -p /root/tmp-cargo
NIK_PROFILE=test \
NIK_SERVER_LISTEN=127.0.0.1:18049 \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_NAME=cloudpivot_so44 \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='lnk03-verify-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
X_CONFIG_SECRET_KEY='lnk03-verify-config-secret-0123456789' \
  setsid ./target/debug/cloudpivot > /root/tmp-cargo/lnk03_instance.log 2>&1 &
echo "started pid=$!"
