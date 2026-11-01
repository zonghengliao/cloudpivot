#!/usr/bin/env bash
# D9 隔离验收库：cloudpivot_d9（99 本机 PG；owner 必须是应用账号 alun）
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=cloudpivot_d9
LOG=/tmp/d9_server.log
MIG_LOG=/tmp/d9_migrate.log

cd /opt/cloudpivot/backend

echo "== 1) 编译 debug 二进制 =="
cargo build --lib --bin cloudpivot 2>&1 | tail -3

echo "== 2) 重建库 $DB =="
runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS $DB"
runuser -u postgres -- psql -qc "CREATE DATABASE $DB OWNER alun"

echo "== 3) 起实例跑迁移 + 种 admin（127.0.0.1:18096）=="
rm -f /tmp/d9_server.pid
NIK_PROFILE=test \
NIK_DATABASE_NAME=$DB \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:18096 \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > $LOG 2>&1 < /dev/null &
echo $! > /tmp/d9_server.pid

for i in $(seq 1 120); do
  if grep -q "数据库迁移完成" $LOG 2>/dev/null; then break; fi
  if ! kill -0 "$(cat /tmp/d9_server.pid)" 2>/dev/null; then echo "实例提前退出"; tail -30 $LOG; exit 1; fi
  sleep 2
done
grep -E "数据库迁移完成|自检通过" $LOG | tail -8

for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:18096/health || true)
  if [ "$code" = "200" ]; then break; fi
  sleep 2
done
echo "health=$code"

echo "== 4) 核对 admin + 迁移版本 =="
export PGPASSWORD=alun
psql -h 127.0.0.1 -U alun -d $DB -tAc "select max(version::int) from schema_migrations"
psql -h 127.0.0.1 -U alun -d $DB -tAc "select u.username from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where r.role_code='SUPER_ADMIN'"
psql -h 127.0.0.1 -U alun -d $DB -tAc "select count(*) from sys_menu where auth_code in ('business:erp:stock-in:create','business:erp:transfer:create','business:task:create')"

echo "== 5) 停实例（按 pid） =="
kill "$(cat /tmp/d9_server.pid)" 2>/dev/null || true
sleep 3
kill -9 "$(cat /tmp/d9_server.pid)" 2>/dev/null || true
echo "done"
