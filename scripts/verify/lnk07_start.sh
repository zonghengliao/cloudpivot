#!/bin/bash
# LNK-07 隔离验收实例：DB=cloudpivot_so44，端口 18054（LNK-06 用 18053，别抢）
#
# 为什么用 cloudpivot_so44：ERP 验收库（250 条迁移齐全，contract_approve 流程已发布）。
# 本卡的全部物证（合同 / 规则 / 通知 / 审批实例）都由 lnk07_curl_evidence.py 自建自清。
set -u
cd /opt/cloudpivot/backend
export PATH=/root/.cargo/bin:$PATH
mkdir -p /root/tmp-cargo
NIK_PROFILE=test \
NIK_SERVER_LISTEN=127.0.0.1:18054 \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_NAME=cloudpivot_so44 \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='lnk07-verify-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
X_CONFIG_SECRET_KEY='lnk07-verify-config-secret-0123456789' \
  setsid ./target/debug/cloudpivot > /root/tmp-cargo/lnk07_instance.log 2>&1 &
echo "started pid=$!"
