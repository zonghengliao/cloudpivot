#!/usr/bin/env bash
# 只读体检：模板库 cloudpivot_fresh 的迁移数 / 业务表是否干净
set -uo pipefail
q() { su postgres -s /bin/sh -c "psql -d $1 -At -c \"$2\""; }
for db in cloudpivot_fresh cloudpivot_dev; do
  echo "== $db"
  q "$db" "select 'migrations='||count(*) from schema_migrations"
  q "$db" "select 'users='||count(*) from sys_user"
  q "$db" "select username||'|status='||status||'|must_change='||must_change_password from sys_user order by username limit 10"
  q "$db" "select 'mat='||(select count(*) from biz_material)||' wh='||(select count(*) from biz_warehouse)||' cust='||(select count(*) from biz_customer)||' so='||(select count(*) from biz_sales_order)||' inv='||(select count(*) from biz_inventory)||' sr='||(select count(*) from biz_sales_return)||' ar='||(select count(*) from biz_receivable)"
done
echo "== 迁移号最大值"
q cloudpivot_fresh "select version from schema_migrations order by version desc limit 3"
