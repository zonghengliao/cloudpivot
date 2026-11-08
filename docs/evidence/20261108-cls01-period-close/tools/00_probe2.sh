#!/usr/bin/env bash
set -uo pipefail
DB=${CLS01_DB:-cloudpivot_cls01}
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
P() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
for t in biz_expense_type biz_asset_category biz_customer biz_material biz_report_item biz_accounting_period biz_opening_balance biz_asset; do
  echo "== $t"
  P "select string_agg(column_name, ', ' order by ordinal_position) from information_schema.columns where table_name='$t'"
done
