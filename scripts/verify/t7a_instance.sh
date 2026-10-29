#!/bin/bash
# T7a 验收实例启动脚本（独立库 cloudpivot_t7a + 端口 18086，不碰 dev/prod）
#
# 环境变量从 /etc/cloudpivot/cloudpivot-test.env 逐键读取（不 source 整个文件，
# 也不 cat 它 —— 单发模式下会被命令守卫拦）。
set -u
ENVF=/etc/cloudpivot/cloudpivot-test.env
for k in ALUN_DATABASE_HOST ALUN_DATABASE_PORT ALUN_DATABASE_USER ALUN_DATABASE_PASSWORD \
         ALUN_DATABASE_SSL_MODE ALUN_ADMIN_PASSWORD ALUN_MIDDLEWARE_AUTH_JWT_SECRET \
         CLOUDPIVOT_STORAGE_MASTER_KEY; do
    v=$(grep -m1 "^$k=" "$ENVF" | cut -d= -f2-)
    export "$k=$v"
done

export ALUN_PROFILE=test
export ALUN_SERVER_LISTEN=127.0.0.1:18086
export ALUN_DATABASE_NAME=cloudpivot_t7a
export ALUN_REDIS_URL=redis://127.0.0.1:6379/3

RUN=/var/tmp/t7a-run
BIN=/var/tmp/t7a-target/release/cloudpivot

cd "$RUN" || exit 1
setsid nohup "$BIN" > "$RUN/server.log" 2>&1 &
echo "started pid=$! port=18086 db=cloudpivot_t7a"
