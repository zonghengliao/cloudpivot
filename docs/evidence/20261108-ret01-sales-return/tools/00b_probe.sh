#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=cloudpivot_ret01
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== roles"; q "select id||' '||role_code||' '||role_name||' '||status from sys_role order by role_code"
echo "== units"; q "select id||' '||unit_code||' '||unit_name from biz_unit"
echo "== warehouse"; q "select id||' '||warehouse_code||' '||warehouse_name from biz_warehouse"
echo "== material"; q "select id||' '||material_code||' '||material_name||' unit='||unit_id||' serial='||serial_required from biz_material"
echo "== account"; q "select account_code||' '||account_name from biz_account where account_code in ('1405','6401','5001','1122','2221.02','6001')"
echo "== admin"; q "select username||' must_change='||must_change_password from sys_user"
echo "== menu sales-return"; q "select path||' '||name||' '||title||' '||coalesce(auth_code,'-') from sys_menu where path like '%sales-return%' or auth_code like 'business:erp:sales-return%'"
