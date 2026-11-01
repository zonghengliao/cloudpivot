#!/usr/bin/env bash
# D5 全量单测：在**干净隔离库** cloudpivot_d5full 上跑 `cargo test --lib`
# （真库用例靠 ERP_DB_URL 开关；库必须已迁移到最新且 admin 已种上，否则审批流用例 65018 假红）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
DB=cloudpivot_d5full
LOG=/tmp/d5_full_suite.log
cd /opt/cloudpivot/backend

echo "== 1) 建库 + 迁移（起一次实例即自动跑完 + 种 admin）=="
runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS $DB"
runuser -u postgres -- psql -qc "CREATE DATABASE $DB OWNER alun"
NIK_PROFILE=test NIK_DATABASE_NAME=$DB NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:18096 NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > /tmp/d5_full_boot.log 2>&1 < /dev/null &
BOOTPID=$!
for i in $(seq 1 90); do
  grep -q "数据库迁移完成" /tmp/d5_full_boot.log 2>/dev/null && break
  sleep 2
done
grep -E "数据库迁移完成|自检通过" /tmp/d5_full_boot.log | tail -3
export PGPASSWORD=alun
for i in $(seq 1 30); do
  admins=$(psql -h 127.0.0.1 -U alun -d $DB -tAc "select count(*) from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where r.role_code='SUPER_ADMIN'")
  [ "${admins:-0}" -ge 1 ] && break
  sleep 2
done
echo "SUPER_ADMIN 账号数 = ${admins:-0}"
kill $BOOTPID 2>/dev/null; sleep 3; kill -9 $BOOTPID 2>/dev/null

echo "== 2) 全量 lib 测试（串行；约 7 分钟）=="
ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/$DB" \
NIK_PROFILE=test \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  cargo test --lib -- --test-threads=1 > $LOG 2>&1
echo "cargo test exit=$?"
grep -E "^test result:" $LOG
grep -E "^(failures:|    business|    system|    shared)" -A 1 $LOG | head -40
