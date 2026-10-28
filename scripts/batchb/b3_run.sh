#!/bin/bash
# 批次 B · B3 验收实例启动脚本（独立运行目录 + 独立 target，避免与主 target / 其它卡串用）
#
# 用法（在部署机上，root）：
#   su postgres -c "createdb -O cloudpivot cloudpivot_b3v"     # 只需一次
#   CARGO_TARGET_DIR=/var/tmp/b3-target cargo build -p cloudpivot
#   setsid nohup scripts/batchb/b3_run.sh > /var/tmp/b3-run.log 2>&1 &
#
# 为什么不能直接 `cargo run`：
#   - `/opt/cloudpivot/backend/target/debug/cloudpivot` 是**共享**输出路径，
#     其它卡一次 build 就会把二进制换成「没有你的路由」的版本（现象：接口 404，而 build 说 up-to-date）；
#   - 验收要独立库（cloudpivot_b3v）+ 独立端口（8099）+ 独立 Redis db（6），
#     否则会污染 dev/prod 与别的卡的夹具。
set -eu

RUN=/var/tmp/b3-run
TARGET=${B3_TARGET_DIR:-/var/tmp/b3-target}
ENVF=/etc/cloudpivot/cloudpivot-test.env
DB=${B3_DB:-cloudpivot_b3v}
PORT=${B3_PORT:-8099}
REDIS_DB=${B3_REDIS_DB:-6}

mkdir -p "$RUN/config"
cp -f "$TARGET/debug/cloudpivot" "$RUN/cloudpivot"
cp -f /opt/cloudpivot/backend/config/*.toml "$RUN/config/" 2>/dev/null || true
ln -sfn /opt/cloudpivot/backend/migrations "$RUN/migrations"
chmod +x "$RUN/cloudpivot"

# 只取白名单键：`source /etc/cloudpivot/*.env` 在部分环境会被命令守卫拦下，
# 且框架只认 ALUN_ 前缀的少数几项（见 alun-config-0.4.2/src/env.rs 的映射表）。
for k in ALUN_PROFILE ALUN_DATABASE_HOST ALUN_DATABASE_USER ALUN_DATABASE_PASSWORD \
         ALUN_DATABASE_SSL_MODE ALUN_ADMIN_PASSWORD ALUN_MIDDLEWARE_AUTH_JWT_SECRET \
         CLOUDPIVOT_STORAGE_MASTER_KEY RUST_LOG; do
  v=$(grep -m1 "^$k=" "$ENVF" | cut -d= -f2-)
  export "$k=$v"
done
export ALUN_DATABASE_NAME="$DB"
export ALUN_SERVER_LISTEN="127.0.0.1:$PORT"
# config-test.toml 没有 [cache] 段（默认 local）；数据权限的 epoch / scope 缓存
# 只在 Redis 后端下才存在，验收 ⑤⑥ 必须切过来（框架只认 ALUN_CACHE_TYPE）。
export ALUN_CACHE_TYPE=redis
export ALUN_REDIS_URL="redis://127.0.0.1:6379/$REDIS_DB"

cd "$RUN"
exec ./cloudpivot
