#!/usr/bin/env bash
# 守卫自测：客户已重新发布 / 手工改图（hash 既不是种子也不是本迁移写的）时，
# V0301 必须**跳过改图**且不抛异常（fail-open 到「不动别人的图」，只补标记）。
set -uo pipefail
DB=${1:-cloudpivot_migtest}
export PGPASSWORD=alun
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -q \
  -c "UPDATE biz_flow_def SET is_system = FALSE WHERE flow_code='so_approve'" \
  -c "UPDATE biz_flow_def_version SET graph_hash = repeat('ab', 32) WHERE id='20000000-0000-4000-8000-0000000d0282'"
echo "== 造出「客户改过」的状态（hash=abab…）=="
psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' \
  -c "select 'flag=' || is_system::text from biz_flow_def where flow_code='so_approve'" \
  -c "select 'hash=' || btrim(graph_hash) from biz_flow_def_version where id='20000000-0000-4000-8000-0000000d0282'"
echo "== 跑 up.sql（应当只打标、不改图）=="
psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -q -f \
  /opt/cloudpivot/backend/migrations/business/V0301__sales_order_flow_is_system_and_snapshot.up.sql
echo "退出码=$?"
psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' \
  -c "select 'flag=' || is_system::text from biz_flow_def where flow_code='so_approve'" \
  -c "select 'hash=' || btrim(graph_hash) from biz_flow_def_version where id='20000000-0000-4000-8000-0000000d0282'"
