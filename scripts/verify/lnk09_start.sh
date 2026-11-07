#!/bin/bash
# LNK-09 隔离验收实例：DB=cloudpivot_lnk09n，端口 18059
# （LNK-06 18053 / LNK-07 18054 / LNK-08 18055，别抢；用完必须杀掉释放端口）
#
# 为什么用 cloudpivot_lnk09n：从 ERP 验收库 cloudpivot_so44 克隆的干净库（250+ 条迁移齐全，
# 资产分类 / 科目 / asset_disposal 流程 / 联动规则表都在）。本卡的全部物证
# （资产卡片 / 处置审批 / 规则 / 通知 / 凭证）都由 lnk09_http_evidence.py 自建自清。
set -u
cd /opt/cloudpivot/backend
export PATH=/root/.cargo/bin:$PATH
export TMPDIR=/root/tmp-cargo
mkdir -p /root/tmp-cargo
NIK_PROFILE=test \
NIK_SERVER_LISTEN=127.0.0.1:18059 \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_NAME=cloudpivot_lnk09n \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='lnk09-verify-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
X_CONFIG_SECRET_KEY='lnk09-verify-config-secret-0123456789' \
  setsid ./target/debug/cloudpivot > /root/tmp-cargo/lnk09_instance.log 2>&1 &
echo "started pid=$!"
