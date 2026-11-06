#!/usr/bin/env bash
# 「修之前」的对照基线：同一冻结迁移集，但**不含** V0301 → 应复现 H6 BLOCKER / H5 WARN。
set -uo pipefail
WS=/root/.hermes/kanban/boards/dev/workspaces/t_1dadde81
SRC=$WS/migrations_frozen
BEFORE=$WS/migrations_before
RUN=$WS/run_env_before
DB=${1:-cloudpivot_so23before}
PORT=18225
EV=/opt/cloudpivot/docs/evidence/20261106-so23-followup-so-approve
export PGPASSWORD=alun
su_psql() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }

rm -rf "$BEFORE" "$RUN"
cp -a "$SRC" "$BEFORE"
rm -f "$BEFORE"/business/V0301*
mkdir -p "$RUN"
cp -a /opt/cloudpivot/backend/config "$RUN/config"
sed -i "s|^path = \"migrations\"|path = \"$BEFORE\"|" "$RUN/config/config.toml"
ln -s "$BEFORE" "$RUN/migrations"
echo "对照基线 up 迁移数：$(ls "$BEFORE"/*/V*.up.sql | wc -l)（不含 V0301）"

su_psql "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
printf 'DROP DATABASE IF EXISTS %s;\nCREATE DATABASE %s OWNER alun;\n' "$DB" "$DB" | su_psql "-f -" >/dev/null

rm -f /tmp/t_1dadde81_before.log
cd "$RUN" || exit 1
NIK_PROFILE=test NIK_DATABASE_NAME="$DB" NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:$PORT NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup /opt/cloudpivot/backend/target/debug/cloudpivot > /tmp/t_1dadde81_before.log 2>&1 < /dev/null &
PID=$!
for _ in $(seq 1 90); do
  if grep -q "数据库迁移完成" /tmp/t_1dadde81_before.log 2>/dev/null; then break; fi
  kill -0 "$PID" 2>/dev/null || { echo "FATAL: 实例提前退出"; tail -20 /tmp/t_1dadde81_before.log; exit 1; }
  sleep 2
done
grep -E "数据库迁移完成" /tmp/t_1dadde81_before.log | tail -1
kill "$PID" 2>/dev/null; sleep 2; kill -9 "$PID" 2>/dev/null

{
  echo "== 对照基线（不含 V0301，库 $DB）=="
  psql -h 127.0.0.1 -U alun -d "$DB" -tA -F'|' \
    -c "select '迁移数=' || count(*) from schema_migrations" \
    -c "select 'so_approve: is_system=' || is_system::text || '，flow_name=' || flow_name from biz_flow_def where flow_code='so_approve'" \
    -c "select 'v1 hash=' || btrim(graph_hash) from biz_flow_def_version where id='20000000-0000-4000-8000-0000000d0282'" \
    -c "select '模板默认口径命中=' || count(*) from biz_flow_def d where d.deleted_at is null and d.is_system and d.flow_code='so_approve'"
  echo
  echo "== erp_invariants --group 审批（应 H6 BLOCKER / H5 WARN）=="
} > "$EV/raw/before-baseline.txt" 2>&1

cd /opt/cloudpivot || exit 1
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME="$DB" \
  timeout 200 python3 scripts/verify/erp_invariants.py --group 审批 >> "$EV/raw/before-baseline.txt" 2>&1
grep -E "H5|H6|^结果：" "$EV/raw/before-baseline.txt"
