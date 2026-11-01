#!/usr/bin/env bash
# R5 隔离验收库：cloudpivot_r5（99 本机 PG；owner = alun）
# 起一次实例让迁移跑完（含 V0189），health 200 后按 pid 停掉。
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=cloudpivot_r5
PORT=18097
LOG=/tmp/r5_server.log

cd /opt/cloudpivot/backend

echo "== 1) 编译 debug 二进制（含 bin，供迁移用）=="
cargo build --bin cloudpivot 2>&1 | tail -3

echo "== 2) 重建库 $DB =="
runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS $DB"
runuser -u postgres -- psql -qc "CREATE DATABASE $DB OWNER alun"

echo "== 3) 起实例跑迁移 + 种 admin（127.0.0.1:$PORT）=="
rm -f /tmp/r5_server.pid
NIK_PROFILE=test \
NIK_DATABASE_NAME=$DB \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:$PORT \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > $LOG 2>&1 < /dev/null &
echo $! > /tmp/r5_server.pid

for i in $(seq 1 150); do
  if grep -q "数据库迁移完成" $LOG 2>/dev/null; then break; fi
  if ! kill -0 "$(cat /tmp/r5_server.pid)" 2>/dev/null; then echo "实例提前退出"; tail -40 $LOG; exit 1; fi
  sleep 2
done
grep -E "数据库迁移完成|V0189|自检通过" $LOG | tail -10

for i in $(seq 1 90); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:$PORT/health || true)
  if [ "$code" = "200" ]; then break; fi
  sleep 2
done
echo "health=$code"

echo "== 4) 核对迁移版本 / 期初节点 / admin =="
export PGPASSWORD=alun
psql -h 127.0.0.1 -U alun -d $DB -tAc "select max(version::int) from schema_migrations"
psql -h 127.0.0.1 -U alun -d $DB -tAc "select count(*) from sys_menu where auth_code like 'business:erp:opening:%'"
psql -h 127.0.0.1 -U alun -d $DB -tAc "select u.username from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where r.role_code='SUPER_ADMIN'"
psql -h 127.0.0.1 -U alun -d $DB -tAc "select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_voucher_type'"
psql -h 127.0.0.1 -U alun -d $DB -tAc "select indexname from pg_indexes where indexname='uk_biz_voucher_opening_period'"

echo "== 5) 停实例（按 pid）=="
kill "$(cat /tmp/r5_server.pid)" 2>/dev/null || true
sleep 3
kill -9 "$(cat /tmp/r5_server.pid)" 2>/dev/null || true
echo "done"
