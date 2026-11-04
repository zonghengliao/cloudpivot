#!/bin/bash
set -uo pipefail
export PGPASSWORD="${ALUN_DATABASE_PASSWORD:-alun}"   # 本机 dev 库口令，允许用环境变量覆盖
S=/root/.hermes/profiles/backend-developer/cache/scratch
DB=cloudpivot_pg239_t
M=/opt/cloudpivot/backend/migrations/business
q() { psql -h 127.0.0.1 -U alun -d "$DB" -At -F'|' -c "$1"; }

echo "== after 1st run"
q "select r.role_code, m.auth_code from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where m.auth_code='business:flow:approve' order by 1"
echo "-- approve total: $(q "select count(*) from sys_role_menu where menu_id='20000000-0000-0000-0000-000000000803'")"
echo "-- instance:read total: $(q "select count(*) from sys_role_menu where menu_id='20000000-0000-0000-0000-000000000801'")"
echo "-- all role_menu total: $(q "select count(*) from sys_role_menu")"

echo "== 2nd run (idempotency)"
psql -v ON_ERROR_STOP=1 -1 -h 127.0.0.1 -U alun -d "$DB" -f $M/V0239__flow_approver_role_grants.up.sql 2>&1 | tail -5
echo "-- all role_menu total after 2nd: $(q "select count(*) from sys_role_menu")"

echo "== down run"
psql -v ON_ERROR_STOP=1 -1 -h 127.0.0.1 -U alun -d "$DB" -f $M/V0239__flow_approver_role_grants.down.sql 2>&1 | tail -5
echo "-- role_menu total after down: $(q "select count(*) from sys_role_menu")"
