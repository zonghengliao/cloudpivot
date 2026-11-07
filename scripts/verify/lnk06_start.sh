#!/bin/bash
# LNK-06 隔离验收实例：DB=cloudpivot_so44，端口 18053（LNK-05 用 18052，别抢）
#
# 为什么用 cloudpivot_so44：ERP 验收库（250 条迁移齐全、科目 1405/2202 与会计期间都在）。
# 本卡的全部物证（手工凭证 / 规则 / 通知）都由 lnk06_curl_evidence.py 自建自清。
set -u
cd /opt/cloudpivot/backend
export PATH=/root/.cargo/bin:$PATH
mkdir -p /root/tmp-cargo
NIK_PROFILE=test \
NIK_SERVER_LISTEN=127.0.0.1:18053 \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_NAME=cloudpivot_so44 \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='lnk06-verify-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
X_CONFIG_SECRET_KEY='lnk06-verify-config-secret-0123456789' \
  setsid ./target/debug/cloudpivot > /root/tmp-cargo/lnk06_instance.log 2>&1 &
echo "started pid=$!"
