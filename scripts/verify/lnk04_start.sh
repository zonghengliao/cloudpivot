#!/bin/bash
# LNK-04 隔离验收入口：起真机实例（DB=cloudpivot_so44，端口 18051）
#
# 为什么用 cloudpivot_so44：它是 ERP 验收库（250 条迁移齐全、主数据与期间都在），
# LNK-03 也是拿它做的真机取证。本卡的全部业务物证（仓 / 物料 / 库存 / 盘点单 / 凭证 /
# 流水 / 规则 / 通知）都由脚本自建自清，不碰库里既有单据。
set -u
cd /opt/cloudpivot/backend
export PATH=/root/.cargo/bin:$PATH
mkdir -p /root/tmp-cargo
NIK_PROFILE=test \
NIK_SERVER_LISTEN=127.0.0.1:18051 \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_NAME=cloudpivot_so44 \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='lnk04-verify-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
X_CONFIG_SECRET_KEY='lnk04-verify-config-secret-0123456789' \
  setsid ./target/debug/cloudpivot > /root/tmp-cargo/lnk04_instance.log 2>&1 &
echo "started pid=$!"
