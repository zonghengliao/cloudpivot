#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=cloudpivot_ret01
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
SQL1="select count(*) from sys_menu where auth_code like 'business:erp:%'"
SQL2="select path||' | '||name||' | '||title||' | '||coalesce(auth_code,'-') from sys_menu where auth_code like 'business:erp:sales-return:%' or path like '%sales%'"
SQL3="select name||' '||coalesce(success::text,'-') from schema_migrations order by applied_seq desc limit 6"
SQL4="select version||' '||name||' '||success from schema_migrations where name like 'V030%' order by name"
echo "menus_total=$(q 'select count(*) from sys_menu')"
echo "erp_auth=$(q "$SQL1")"
echo "== sales_return_rows"; q "$SQL2"
echo "== migration_tail"; q "$SQL3"
echo "== migration_030x"; q "$SQL4"
