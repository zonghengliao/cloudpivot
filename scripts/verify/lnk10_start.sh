#!/bin/bash
# LNK-10 隔离验收实例：DB=cloudpivot_lnk09n（LNK-09 留下的干净库，252 条迁移齐全），端口 18061
# （LNK-06 18053 / LNK-07 18054 / LNK-08 18055 / LNK-09 18059，别抢；用完必须杀掉释放端口）
#
# 二进制取隔离 worktree 的产物（/root/target-lnk10/debug/cloudpivot），配置 / 迁移目录用
# /opt/cloudpivot/backend（`config/config-test.toml` + migrations 都在那里）：
# 二者在 LNK-10 的改动上等价（同一份源码 + 本卡 7 个文件）。
set -u
cd /opt/cloudpivot/backend
export PATH=/root/.cargo/bin:$PATH
export TMPDIR=/root/tmp-cargo
mkdir -p /root/tmp-cargo
NIK_PROFILE=test \
NIK_SERVER_LISTEN=127.0.0.1:18061 \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_NAME=cloudpivot_lnk09n \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='lnk10-verify-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
X_CONFIG_SECRET_KEY='lnk10-verify-config-secret-0123456789' \
  setsid /root/target-lnk10/debug/cloudpivot > /root/tmp-cargo/lnk10_instance.log 2>&1 &
echo "started pid=$!"
