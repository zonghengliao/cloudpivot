#!/bin/bash
set -uo pipefail
export PGPASSWORD="${ALUN_DATABASE_PASSWORD:-alun}"   # 本机 dev 库口令，允许用环境变量覆盖
S=/root/.hermes/profiles/backend-developer/cache/scratch
DB=cloudpivot_pg239_t
su postgres -c "createdb -O alun $DB" || { echo "createdb FAILED"; exit 1; }
export PGPASSWORD="${ALUN_DATABASE_PASSWORD:-alun}"   # 本机 dev 库口令，允许用环境变量覆盖
pg_dump -h 127.0.0.1 -U alun -d cloudpivot_manual2 --no-owner --no-privileges -Fp > $S/base.sql || { echo "pg_dump FAILED"; exit 1; }
ls -l $S/base.sql
psql -q -h 127.0.0.1 -U alun -d $DB -f $S/base.sql > $S/load.log 2>&1 || { echo "load FAILED"; tail -5 $S/load.log; exit 1; }
echo "clone rows: $(psql -h 127.0.0.1 -U alun -d $DB -Atc 'select count(*) from sys_role_menu')"
