#!/usr/bin/env bash
# SO-24 对照：干净模板库（不经 e2e 链路）的全量巡检基线
set -uo pipefail
export PGPASSWORD=alun
DB=cloudpivot_so24ctl
psql_super() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }
psql_super "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
psql_super "-c 'DROP DATABASE IF EXISTS $DB'" >/dev/null
psql_super "-c 'CREATE DATABASE $DB TEMPLATE cloudpivot_so23fresh OWNER alun'" >/dev/null
cd /opt/cloudpivot
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME="$DB" \
  python3 scripts/verify/erp_invariants.py > docs/evidence/sales-e2e/raw/control_pristine_full.txt 2>&1
grep -E '结果：' docs/evidence/sales-e2e/raw/control_pristine_full.txt | tail -1
grep -E '🚨|❌' docs/evidence/sales-e2e/raw/control_pristine_full.txt | head -6
