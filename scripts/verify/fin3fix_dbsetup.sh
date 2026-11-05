#!/usr/bin/env bash
# FIN-3 缺陷回归用干净库：模板 cloudpivot_fin2asset（V0238），迁移由实例启动时自动补齐。
set -uo pipefail
DB=${1:-cloudpivot_fin3fix}
TEMPLATE=${2:-cloudpivot_fin2asset}
export PGPASSWORD=alun
runuser -u postgres -- psql -d postgres -q -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS $DB" \
  -c "CREATE DATABASE $DB TEMPLATE $TEMPLATE OWNER alun" || exit 1
psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' -c "
select 'tables|' || count(*) from information_schema.tables where table_schema='public'
union all select 'max_migration|' || coalesce(max(version)::text,'-') from schema_migrations
union all select 'admin|' || count(*) from sys_user where username='admin'"
