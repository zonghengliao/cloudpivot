#!/bin/bash
# LNK-08 隔离验收实例：DB=cloudpivot_lnk08，端口 18055（LNK-06 用 18053 / LNK-07 用 18054，别抢）
#
# 为什么用 cloudpivot_lnk08：从 ERP 验收库 cloudpivot_so44 克隆的干净库（250 条迁移齐全，
# 期间 / 凭证 / 联动规则表都在）。本卡的全部物证（期间 / 规则 / 通知）都由
# lnk08_http_evidence.py 自建自清。
set -u
cd /opt/cloudpivot/backend
export PATH=/root/.cargo/bin:$PATH
export TMPDIR=/root/tmp-cargo
mkdir -p /root/tmp-cargo
NIK_PROFILE=test \
NIK_SERVER_LISTEN=127.0.0.1:18055 \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_NAME=cloudpivot_lnk08 \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='lnk08-verify-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
X_CONFIG_SECRET_KEY='lnk08-verify-config-secret-0123456789' \
  setsid ./target/debug/cloudpivot > /root/tmp-cargo/lnk08_instance.log 2>&1 &
echo "started pid=$!"
