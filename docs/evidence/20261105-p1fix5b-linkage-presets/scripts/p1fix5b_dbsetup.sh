#!/usr/bin/env bash
# P1-FIX-5B：隔离库（owner = 应用账号 alun）。
#
#   bash p1fix5b_dbsetup.sh                 # 建 cloudpivot_lnk5b（已存在则跳过）
#   RECREATE=1 bash p1fix5b_dbsetup.sh      # 先删后建（从零跑完整迁移链，含 V0254）
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

DB=${LNK_DB:-cloudpivot_lnk5b}

if [ "${RECREATE:-0}" = "1" ] && PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d postgres -Atc \
    "select 1 from pg_database where datname='$DB'" | grep -q 1; then
  su - postgres -c "dropdb --force $DB" && echo "dropped: $DB"
fi

if PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d postgres -Atc \
    "select 1 from pg_database where datname='$DB'" | grep -q 1; then
  echo "已存在：$DB（RECREATE=1 可重建）"
else
  su - postgres -c "createdb -O alun $DB" && echo "created: $DB"
fi

PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -Atc "select current_database(), current_user"
