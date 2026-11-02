#!/usr/bin/env bash
# D2：建一个干净隔离库（owner = 应用账号 alun），供迁移 + 单测 / 不变量用。
#
#   bash scripts/verify/d2_dbsetup.sh              # 建 cloudpivot_d2（已存在则跳过）
#   D2_DB=cloudpivot_d2inv bash …/d2_dbsetup.sh    # 换库名
#   RECREATE=1 bash …/d2_dbsetup.sh                # 先删后建（全量单测前清场）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

DB=${D2_DB:-cloudpivot_d2}

if [ "${RECREATE:-0}" = "1" ] && PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d postgres -Atc \
    "select 1 from pg_database where datname='$DB'" | grep -q 1; then
  su - postgres -c "dropdb --force $DB" && echo "dropped: $DB"
fi

if PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d postgres -Atc "select 1 from pg_database where datname='$DB'" | grep -q 1; then
  echo "已存在：$DB（RECREATE=1 可重建）"
else
  su - postgres -c "createdb -O alun $DB" && echo "created: $DB"
fi

PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc "select current_database(), current_user"
