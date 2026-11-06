#!/usr/bin/env bash
# 迁移双向自测（数据层，不动 schema_migrations）：down → 回到种子字节；up → 回到 canonical。
set -uo pipefail
DB=${1:-cloudpivot_migtest}
export PGPASSWORD=alun
su_psql() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }
su_psql "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
cat > /tmp/_migtest.sql <<SQL
DROP DATABASE IF EXISTS $DB;
CREATE DATABASE $DB TEMPLATE cloudpivot_so23fresh OWNER alun;
SQL
su_psql "-f -" < /tmp/_migtest.sql >/dev/null || exit 1

show() {
  psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' \
    -c "select 'flag=' || is_system::text from biz_flow_def where flow_code='so_approve'" \
    -c "select 'hash=' || btrim(graph_hash) from biz_flow_def_version where id='20000000-0000-4000-8000-0000000d0282'" \
    -c "select 'edges=' || string_agg(x.item->>'key', ',' order by x.ord)
          from biz_flow_def_version v,
               jsonb_array_elements(v.graph_snapshot->'edges') with ordinality as x(item, ord)
         where v.id='20000000-0000-4000-8000-0000000d0282'"
}

echo "== 应用 V0301 之后（canonical 口径）=="; show
echo "== 跑 down.sql =="
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -q -f \
  /opt/cloudpivot/backend/migrations/business/V0301__sales_order_flow_is_system_and_snapshot.down.sql
show
echo "== 再跑 up.sql（幂等/可重放）=="
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -q -f \
  /opt/cloudpivot/backend/migrations/business/V0301__sales_order_flow_is_system_and_snapshot.up.sql
show
echo "== 再跑一次 up.sql（幂等：0 行变更、不报错）=="
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -q -f \
  /opt/cloudpivot/backend/migrations/business/V0301__sales_order_flow_is_system_and_snapshot.up.sql
show
