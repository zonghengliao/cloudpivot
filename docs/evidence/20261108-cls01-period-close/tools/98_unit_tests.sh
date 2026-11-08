#!/usr/bin/env bash
# CLS-01 单测（真库，本机 99）：period 模块 —— 新增年结护栏纯逻辑用例 + 16 条真库用例
#   145 只有 7G 内存，链接 700M 的 test 二进制会被 OOM SIGKILL；99 有 15G，本地 PG 在 127.0.0.1:5432。
#   库：cloudpivot_cls01_ut（owner alun），迁移由实例首启跑。
set -uo pipefail
DB=cloudpivot_cls01_ut
PORT=18230
BIN=/opt/cloudpivot/backend/target/debug/cloudpivot
LOG=/tmp/cls01_ut99_server.log
export PATH=/root/.cargo/bin:$PATH

su - postgres -c "psql -q -At -c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null 2>&1
su - postgres -c "psql -q -At -c \"DROP DATABASE IF EXISTS $DB\"" >/dev/null 2>&1
su - postgres -c "psql -q -At -c \"CREATE DATABASE $DB OWNER alun\"" >/dev/null || { echo "FATAL 建库失败"; exit 1; }
echo "✅ 空库 $DB 已建（owner alun）"

cd /opt/cloudpivot/backend || exit 1
export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_NAME="$DB" \
       NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
       NIK_SERVER_LISTEN="127.0.0.1:$PORT" NIK_ADMIN_PASSWORD='Admin@123456' \
       NIK_MIDDLEWARE_AUTH_JWT_SECRET='cls01-local-unit-test-secret-0123456789abcdef' \
       CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
       X_CONFIG_SECRET_KEY='cls01-local-config-secret'
setsid nohup "$BIN" >"$LOG" 2>&1 </dev/null &
PID=$!
code=""
for _ in $(seq 1 150); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && break
  if ! kill -0 "$PID" 2>/dev/null; then echo "FATAL 实例提前退出"; tail -20 "$LOG"; exit 1; fi
  sleep 2
done
kill "$PID" 2>/dev/null; sleep 2; kill -9 "$PID" 2>/dev/null
echo "health=$code"
export PGPASSWORD=alun
echo "migrations=$(psql -h 127.0.0.1 -U alun -At -d "$DB" -c 'select count(*) from schema_migrations where success')"
echo "users=$(psql -h 127.0.0.1 -U alun -At -d "$DB" -c 'select count(*) from sys_user')"
[ "$code" = "200" ] || { echo "FATAL 迁移实例没起来"; tail -20 "$LOG"; exit 1; }

export ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/$DB"
echo "--- cargo test --lib business::erp::period（真库 $DB，test-threads=1）"
cargo test --lib business::erp::period -- --test-threads=1 2>&1 | tail -30
