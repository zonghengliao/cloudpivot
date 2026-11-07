#!/bin/bash
# SO-44 迁移 APPLY_OK 的干净库复证：新建空库 → 起实例自动跑全部迁移 → 断言 318 落库 + 自检通过
set -u
DB=cloudpivot_so44_apply
LOG=/root/tmp-cargo/so44_apply.log
export PATH=/root/.cargo/bin:$PATH

su - postgres -c "dropdb --if-exists $DB" 2>/dev/null
su - postgres -c "createdb -O alun $DB" || { echo "createdb 失败"; exit 1; }
echo "== 空库已建：$DB（owner=alun，无 schema_migrations）"

cd /opt/cloudpivot/backend || exit 1
: > "$LOG"
NIK_PROFILE=test \
NIK_SERVER_LISTEN=127.0.0.1:18045 \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_NAME=$DB \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='so44-apply-secret-0123456789abcdefghijklmnopqrstuvwxyz' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
X_CONFIG_SECRET_KEY='so44-apply-config-secret-0123456789' \
  setsid ./target/debug/cloudpivot > "$LOG" 2>&1 &
echo "== 实例已起（18045），等迁移 + bootstrap"

for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18045/health 2>/dev/null)
  [ "$code" = "200" ] && break
  sleep 2
done
echo "== /health = ${code:-timeout}"

echo "== 迁移日志"
grep -n '迁移' "$LOG" | tail -3

echo "== V0318 记录"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -tAc \
  "select version, name, kind, applied_via, success from schema_migrations where version = '318'"

echo "== 表结构"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -tAc \
  "select 'target_cols=' || (select count(*) from information_schema.columns where table_name='biz_sales_target')
        || ' forecast_cols=' || (select count(*) from information_schema.columns where table_name='biz_sales_forecast')
        || ' checks=' || (select count(*) from pg_constraint where conname like 'ck_biz_st%' or conname like 'ck_biz_sf%')
        || ' part_uk=' || (select count(*) from pg_indexes where indexname in ('uk_biz_sales_target_no','uk_biz_sales_target_scope','uk_biz_sales_forecast_no','uk_biz_sales_forecast_dim'))
        || ' seq=' || (select count(*) from biz_seq where seq_type in ('STG','SFC') and deleted_at is null)
        || ' menus=' || (select count(*) from sys_menu where auth_code like 'business:erp:sales-%')"

echo "== 自检 NOTICE"
grep -n 'SO-44: 销售目标' "$LOG" | tail -2

PID=$(ss -lntp 2>/dev/null | grep 18045 | grep -o 'pid=[0-9]*' | cut -d= -f2 | head -1)
[ -n "$PID" ] && kill "$PID" && echo "== 实例已停（pid=$PID）"
