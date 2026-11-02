#!/usr/bin/env bash
# ERP-B2 隔离验收库：erpb2_e2e（本机 PG，owner alun）+ 127.0.0.1:18097 实例
#
#   bash scripts/verify/b2_dbsetup.sh           # 重建库 + 起实例跑迁移（迁移完停掉）
#   bash scripts/verify/b2_dbsetup.sh keep      # 重建库 + 起实例并**保持运行**（接着跑 e2e）
#
# 与 d5_dbsetup.sh 同一套约定，只换了库名 / 端口 / 日志路径（/tmp 是 tmpfs，日志落 /root/tmp）。
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH
export TMPDIR=/root/tmp
DB=erpb2_e2e
PORT=18097
LOG=/root/tmp/b2_server.log
PIDFILE=/root/tmp/b2_server.pid
MODE=${1:-stop}
mkdir -p /root/tmp

cd /opt/cloudpivot/backend

echo "== 1) 编译 debug 二进制"
cargo build --lib --bin cloudpivot 2>&1 | tail -3

echo "== 2) 重建库 $DB"
runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS $DB"
runuser -u postgres -- psql -qc "CREATE DATABASE $DB OWNER alun"

echo "== 3) 起实例跑迁移（:$PORT，日志 $LOG）"
rm -f "$PIDFILE"
NIK_PROFILE=test \
NIK_DATABASE_NAME=$DB \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:$PORT \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"

for _ in $(seq 1 120); do
  if grep -q "数据库迁移完成" "$LOG" 2>/dev/null; then break; fi
  if ! kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then echo "实例提前退出"; tail -40 "$LOG"; exit 1; fi
  sleep 2
done
grep -E "数据库迁移完成|自检通过" "$LOG" | tail -5

for _ in $(seq 1 90); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
  if [ "$code" = "200" ]; then break; fi
  sleep 2
done
echo "health=$code"

echo "== 4) 核对 V0196 + 四条参数 + admin"
export PGPASSWORD=alun
psql -h 127.0.0.1 -U alun -d $DB -tAc "select max(version::int) from schema_migrations"
psql -h 127.0.0.1 -U alun -d $DB -tAc \
  "select config_key || '=' || config_value || ' (' || config_type || ',' || category || ')' from sys_config where config_key like 'erp.separation_of_duties.%' order by sort_order"
psql -h 127.0.0.1 -U alun -d $DB -tAc \
  "select u.username from sys_user u join sys_user_role ur on ur.user_id=u.id join sys_role r on r.id=ur.role_id where r.role_code='SUPER_ADMIN'"

if [ "$MODE" = "keep" ]; then
  echo "实例保持运行：pid $(cat "$PIDFILE") @ http://127.0.0.1:$PORT"
else
  echo "== 5) 停实例（按 pid）"
  kill "$(cat "$PIDFILE")" 2>/dev/null || true
  sleep 3
  kill -9 "$(cat "$PIDFILE")" 2>/dev/null || true
  echo "done"
fi
