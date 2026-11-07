#!/bin/bash
# LNK-11 隔离验收实例：DB=cloudpivot_lnk11（空库首启，迁移全链由实例自己跑），端口 18062
# （LNK-06 18053 / LNK-07 18054 / LNK-08 18055 / LNK-09 18059 / LNK-10 18061，别抢；用完必须杀掉释放端口）
#
# 二进制取隔离 worktree 的产物（/root/lnk11-target/debug/cloudpivot），配置 / 迁移目录用同一棵
# 隔离 worktree（`config/config-test.toml` + migrations 都在里面）：
# 二者在 LNK-11 的改动上等价（同一份源码 = HEAD + 本卡 7 个文件）。
#
# 用法：bash scripts/verify/lnk11_start.sh
set -u
ROOT="${LNK11_ROOT:-/root/lnk11-wt/backend}"
BIN="${LNK11_BIN:-/root/lnk11-target/debug/cloudpivot}"
LOG="${LNK11_LOG:-/root/lnk11_instance.log}"
cd "$ROOT" || exit 1
export PATH=/root/.cargo/bin:$PATH
export TMPDIR=/root/tmp-cargo
mkdir -p /root/tmp-cargo
NIK_PROFILE=test \
NIK_SERVER_LISTEN=127.0.0.1:18062 \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_NAME=cloudpivot_lnk11 \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='lnk11-verify-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
X_CONFIG_SECRET_KEY='lnk11-verify-config-secret-0123456789' \
  setsid "$BIN" > "$LOG" 2>&1 &
echo "started pid=$!  log=$LOG  db=cloudpivot_lnk11  listen=127.0.0.1:18062"
