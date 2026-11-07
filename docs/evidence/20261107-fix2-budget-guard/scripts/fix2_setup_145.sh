#!/usr/bin/env bash
# FIX-2 隔离实例（145 本机）：新库 cloudpivot_fix2c + env + 起进程 127.0.0.1:8039
set -uo pipefail
DB=cloudpivot_fix2c
BIN=/opt/cloudpivot/backend/target/debug/cloudpivot-fix2
mkdir -p /root/fix2/evidence
PGP=$(grep -m1 ^NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2)

echo "== 1. 建库（owner=cloudpivot，已存在则跳过）=="
su - postgres -c "psql -tAc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
  && echo "  库已存在：$DB" \
  || { su - postgres -c "psql -c \"create database $DB owner cloudpivot\"" ; echo "  已建库：$DB"; }

echo "== 2. 写 env =="
JA=$(openssl rand -base64 48 | tr -d '\n')
SK=$(openssl rand -hex 32)
XS=$(openssl rand -hex 24)
cat > /root/fix2/env <<EOF
# FIX-2 隔离实例（145）：库 $DB / 端口 8039 / Redis DB 8
NIK_PROFILE=dev
NIK_SERVER_LISTEN=127.0.0.1:8039

NIK_DATABASE_HOST=127.0.0.1
NIK_DATABASE_PORT=5432
NIK_DATABASE_NAME=$DB
NIK_DATABASE_USER=cloudpivot
NIK_DATABASE_PASSWORD=$PGP
NIK_DATABASE_SSL_MODE=prefer

NIK_REDIS_URL=redis://127.0.0.1:6379/8
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$JA
NIK_ADMIN_PASSWORD=Fix2Admin9x
NIK_AUTH_QUERY_TOKEN_PATHS=/api/business/notice/stream

RUST_LOG=info

CLOUDPIVOT_STORAGE_MASTER_KEY=$SK
X_CONFIG_SECRET_KEY=$XS
NIK_SEED_DEMO_USERS=true
NIK_DEMO_PASSWORD=Demo@Fix2x9
EOF
chmod 600 /root/fix2/env
echo "  env 写好（密钥掩码）：$(grep -c . /root/fix2/env) 行"

echo "== 3. 起进程 =="
pkill -f "cloudpivot-fix2" 2>/dev/null && sleep 1
cd /opt/cloudpivot/backend || exit 1
set -a; . /root/fix2/env; set +a
nohup "$BIN" > /root/fix2/run.log 2>&1 &
echo "  pid=$!"
for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8039/health)
  [ "$code" = "200" ] && { echo "  health=200（第 ${i} 次探测）"; break; }
  sleep 2
done
curl -s http://127.0.0.1:8039/health; echo
tail -5 /root/fix2/run.log
