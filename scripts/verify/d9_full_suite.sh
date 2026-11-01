#!/usr/bin/env bash
# D9 全量真库套件：干净隔离库 cloudpivot_d9full（V0137–V0188 全量迁移）上跑 cargo test --lib
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=cloudpivot_d9full
LOG=/tmp/d9_full_suite.log
MIGLOG=/tmp/d9_full_migrate.log

cd /opt/cloudpivot/backend

echo "== 1) 重建库 $DB =="
runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS $DB"
runuser -u postgres -- psql -qc "CREATE DATABASE $DB OWNER alun"

echo "== 2) 起一次实例跑迁移（127.0.0.1:18097）=="
rm -f /tmp/d9_full.pid
NIK_PROFILE=test \
NIK_DATABASE_NAME=$DB \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:18097 \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > $MIGLOG 2>&1 < /dev/null &
echo $! > /tmp/d9_full.pid
for i in $(seq 1 120); do
  if grep -q "数据库迁移完成" $MIGLOG 2>/dev/null; then break; fi
  if ! kill -0 "$(cat /tmp/d9_full.pid)" 2>/dev/null; then echo "实例提前退出"; tail -20 $MIGLOG; exit 1; fi
  sleep 2
done
# /health 200 才代表 bootstrap（admin + 角色）种完
for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:18097/health || true)
  if [ "$code" = "200" ]; then break; fi
  sleep 2
done
echo "health=$code"
grep -E "数据库迁移完成" $MIGLOG | tail -1
kill "$(cat /tmp/d9_full.pid)" 2>/dev/null || true
sleep 3
kill -9 "$(cat /tmp/d9_full.pid)" 2>/dev/null || true

echo "== 3) 全量 lib 套件（--test-threads=1）=="
export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/$DB"
export TASK_DB_URL="$ERP_DB_URL"
export SYS_DB_URL="$ERP_DB_URL"
export NIK_PROFILE=test
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
cargo test --lib -- --test-threads=1 > $LOG 2>&1
echo "exit=$?"
grep -E "^test result:" $LOG
grep -E "^(failures:|    [a-z_:]+)$" $LOG | head -40
tail -5 $LOG
