#!/usr/bin/env bash
# D1：在隔离库 cloudpivot_d1 上把迁移跑完（复用 target/debug/cloudpivot 的启动迁移），
# 然后核对 V0204 已应用 + 表结构就绪，最后停掉实例。
#
# 用法：bash scripts/verify/d1_migrate.sh [端口]
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=/opt/cloudpivot
DB=${D1_DB:-cloudpivot_d1}
PORT=${1:-18093}
LOG=/root/d1-migrate.log
PIDFILE=/root/d1-migrate.pid

export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_PASSWORD=alun
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD='Admin@123456'

rm -f "$LOG"
cd "$ROOT/backend" || exit 1
setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"
echo "启动 pid=$(cat "$PIDFILE") 库=$DB 端口=$PORT 日志=$LOG"

ok=0
for _ in $(seq 1 120); do
  if grep -q "数据库迁移完成" "$LOG" 2>/dev/null; then ok=1; break; fi
  if ! kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then break; fi
  sleep 1
done

echo "---- 迁移日志（尾部）----"
tail -6 "$LOG"

echo "---- schema_migrations 最大版本 ----"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "select max(version::int), count(*) from schema_migrations"

echo "---- biz_flow_state_edge 结构 ----"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -At -F'|' -c \
  "select column_name, data_type, is_nullable from information_schema.columns
    where table_name='biz_flow_state_edge' order by ordinal_position"

echo "---- 唯一约束 / 索引 ----"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "select conname from pg_constraint where conrelid='biz_flow_state_edge'::regclass
   union all select indexname from pg_indexes where tablename='biz_flow_state_edge' order by 1"

echo "---- 存活配置行数（应为 0）----"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "select count(*) from biz_flow_state_edge where deleted_at is null"

# 停实例（按 pid 文件，别 pkill）
if kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
  kill "$(cat "$PIDFILE")"
  for _ in $(seq 1 30); do
    kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
    sleep 1
  done
fi
if [ "$ok" = 1 ]; then echo "RESULT: 迁移完成"; else echo "RESULT: 迁移未完成（看 $LOG）"; exit 1; fi
