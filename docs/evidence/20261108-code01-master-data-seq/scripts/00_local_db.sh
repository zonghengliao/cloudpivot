#!/bin/bash
# CODE-01 真库测试库（99 本机）：建库 + 确认 owner
set -u
DB=cloudpivot_code01
exists=$(su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"")
if [ "$exists" = "1" ]; then
  echo "库已存在：$DB"
else
  su - postgres -c "createdb -O alun $DB"
  echo "已建库：$DB（owner=alun）"
fi
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc "select current_database(), current_user, (select count(*) from information_schema.tables where table_schema='public')"
