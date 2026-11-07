#!/usr/bin/env bash
# FIX-4 取证辅助：重新建一份「迁移前」的存量库拷贝（cloudpivot_dev 的备份 dump）
# 用途：33_legacy_migration.sh 要证明「4 条未分配桶 → 0 条」的迁移瞬间，跑过一次就被并掉了，
#      所以每次取证前重建拷贝。
set -euo pipefail
DUMP=$(ls -t /root/backup/cloudpivot_dev_FIX4_*.dump | head -1)
TS=$(date +%Y%m%d-%H%M%S)
DB="cloudpivot_fix4legacy_${TS//-/}"
export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
psql -h 127.0.0.1 -U cloudpivot -d postgres -c "CREATE DATABASE $DB OWNER cloudpivot" > /dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\"" > /dev/null
pg_restore -h 127.0.0.1 -U cloudpivot -d "$DB" --no-owner --no-privileges "$DUMP"
echo "$DB" > /root/backup/.fix4_db_legacy
echo "db_legacy=$DB (from $DUMP)"
