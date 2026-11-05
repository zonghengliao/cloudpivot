#!/usr/bin/env bash
# P1-FIX-9 验收：隔离实例启动（盘点 / 期初）
set -u
DB=cloudpivot_st9
PORT=18190
BIN=/root/st9-bin/cloudpivot
LOG=/root/st9-instance.log

mkdir -p /root/st9-bin
cp -f /opt/cloudpivot/backend/target/debug/cloudpivot "$BIN"
chmod +x "$BIN"

# 停掉旧实例
pkill -f "$BIN" 2>/dev/null
sleep 1

su - postgres -c "psql -tAc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 || \
  su - postgres -c "createdb -O alun $DB"

cd /opt/cloudpivot/backend || exit 1
NIK_PROFILE=test \
NIK_DATABASE_NAME=$DB \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_PORT=5432 \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:$PORT \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='st9-acceptance-jwt-secret-0123456789abcdef' \
CLOUDPIVOT_STORAGE_MASTER_KEY='9f0d1c8b7a6e5d4c3b2a1908f7e6d5c4b3a29180ffeeddccbbaa998877665544' \
nohup "$BIN" >"$LOG" 2>&1 &
echo "started pid=$!"
