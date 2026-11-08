#!/usr/bin/env bash
set -uo pipefail
DB=${CLS01_DB:-cloudpivot_cls01}
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
P() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -F ' | ' -d "$DB" -c "$1" 2>&1; }
P "select item_code, item_name, side, coalesce(account_pattern,'-'), coalesce(formula,'-') from biz_report_item where report_code='INCOME_STATEMENT' order by sort_order"
