#!/usr/bin/env bash
# 证据：**当前仓库原始迁移集**（含 V0267__sales_menu）在空库上跑不完 —— 卡在 267。
# 做法：不剔任何文件，原样跑一次全量迁移，记录失败点与最后入库的版本。
set -uo pipefail
WS=/root/.hermes/kanban/boards/dev/workspaces/t_1dadde81
EV=/opt/cloudpivot/docs/evidence/20261106-so23-followup-so-approve
DB=cloudpivot_v267probe
PORT=18227
RUN=$WS/run_env_raw
export PGPASSWORD=alun
su_psql() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }

rm -rf "$RUN"; mkdir -p "$RUN"
cp -a /opt/cloudpivot/backend/config "$RUN/config"
sed -i "s|^path = \"migrations\"|path = \"/opt/cloudpivot/backend/migrations\"|" "$RUN/config/config.toml"

su_psql "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
printf 'DROP DATABASE IF EXISTS %s;\nCREATE DATABASE %s OWNER alun;\n' "$DB" "$DB" | su_psql "-f -" >/dev/null

cd "$RUN" || exit 1
NIK_PROFILE=test NIK_DATABASE_NAME="$DB" NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:$PORT NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup /opt/cloudpivot/backend/target/debug/cloudpivot > /tmp/t_1dadde81_raw.log 2>&1 < /dev/null &
PID=$!
for _ in $(seq 1 90); do
  kill -0 "$PID" 2>/dev/null || break
  grep -qE "数据库迁移完成|数据库迁移失败" /tmp/t_1dadde81_raw.log 2>/dev/null && break
  sleep 2
done
sleep 2; kill "$PID" 2>/dev/null; sleep 1; kill -9 "$PID" 2>/dev/null

{
  echo "== 原始迁移集（含 V0267/v0301，不剔任何文件）在空库 $DB 上跑全量迁移 =="
  grep -E "数据库迁移完成|数据库迁移失败|FATAL: 服务启动失败" /tmp/t_1dadde81_raw.log | head -3
  echo
  echo "-- 失败点：schema_migrations 最后入库的版本 --"
  psql -h 127.0.0.1 -U alun -d "$DB" -tA -F'|' \
    -c "select '已入库 ' || count(*) || ' 条' from schema_migrations" \
    -c "select '最后一条 = V' || version || ' ' || name from schema_migrations order by applied_seq desc limit 1"
  echo
  echo "-- 直接单跑 V0267 拿真实报错（应用只报 'db error'，psql 才有全文）--"
  psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=0 \
    -f /opt/cloudpivot/backend/migrations/business/V0267__sales_menu.up.sql 2>&1 \
    | grep -iE "ERROR" | head -3
  echo
  echo "-- 那些权限码的菜单承载行由更高号段的迁移建（所以空库上 267 一定先于它们）--"
  grep -lE "business:erp:receivable:list|business:erp:invoice:list" \
    /opt/cloudpivot/backend/migrations/business/*.up.sql | xargs -n1 basename | sort | tr '\n' ' '
  echo
} > "$EV/raw/v0267-fresh-blocker.txt" 2>&1
cat "$EV/raw/v0267-fresh-blocker.txt"
