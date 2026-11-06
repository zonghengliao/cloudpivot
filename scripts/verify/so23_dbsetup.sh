#!/usr/bin/env bash
# SO-23：建正向库 / 演练库（模板复制 cloudpivot_so22fresh2）
# 说明：alun 无 CREATEDB 权限，建库走本机 postgres 超级用户（`su postgres`，peer 认证）。
set -uo pipefail
for db in cloudpivot_so23 cloudpivot_so23neg; do
  su postgres -s /bin/sh -c "psql -tAc \"select 1 from pg_database where datname='$db'\"" > /tmp/so23_exists.txt
  if [ "$(cat /tmp/so23_exists.txt)" = "1" ]; then
    echo "== $db 已存在 → 重建"
    su postgres -s /bin/sh -c "psql -d postgres -tAc \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$db'\"" >/dev/null
    su postgres -s /bin/sh -c "psql -d postgres -tAc 'DROP DATABASE $db'" || exit 1
  fi
  su postgres -s /bin/sh -c "psql -d postgres -tAc 'CREATE DATABASE $db TEMPLATE cloudpivot_so22fresh2 OWNER alun'" || exit 1
  echo "== $db 建好"
done

export PGPASSWORD=alun
for db in cloudpivot_so23 cloudpivot_so23neg; do
  echo "--- $db"
  psql -h 127.0.0.1 -U alun -d "$db" -tAF'|' -c "select count(*) as migrations,
      (select count(*) from schema_migrations where applied_via not in ('executed','imported')) as bad_applied_via
      from schema_migrations"
  psql -h 127.0.0.1 -U alun -d "$db" -tAF'|' -c "select (select count(*) from biz_sales_order) so,
      (select count(*) from biz_shipment) sh,
      (select count(*) from biz_stock_out) out_doc,
      (select count(*) from biz_receivable) ar,
      (select count(*) from biz_invoice) iv"
done
