#!/usr/bin/env bash
# REVERIFY（t_2f92ee87）145 准备：拷贝库 + 最新二进制副本
# 口径同 FIX-5..9：8033 服役实例不动；用 cloudpivot_dev 的拷贝库 + 隔离端口 8050
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot
echo "HEAD=$(git rev-parse --short HEAD) $(git log -1 --format=%s)"
echo "backend 代码与 FIX-9 提交 2cc2eac 的差异行数：$(git diff --stat 2cc2eac..HEAD -- backend/ | wc -l)"
echo "工作区脏文件：$(git status --short | wc -l)"
install -m 0755 backend/target/debug/cloudpivot backend/target/debug/cloudpivot-rev
echo "binary_bytes=$(stat -c %s backend/target/debug/cloudpivot-rev)"

export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
mkdir -p /root/backup
TS=$(date +%Y%m%d-%H%M%S)
DUMP=/root/backup/cloudpivot_dev_REV_${TS}.dump
pg_dump -h 127.0.0.1 -U cloudpivot -Fc -f "$DUMP" cloudpivot_dev
echo "backup=$DUMP ($(du -h "$DUMP" | cut -f1))"

DB="cloudpivot_rev_${TS//-/}"
psql -h 127.0.0.1 -U cloudpivot -d postgres -c "CREATE DATABASE $DB OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\""
pg_restore -h 127.0.0.1 -U cloudpivot -d "$DB" --no-owner --no-privileges "$DUMP"
echo "$DB" > /root/backup/.rev_db
echo "db=$DB"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select 'migrations='||count(*) from schema_migrations"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select 'max_version='||max(version) from schema_migrations where version ~ '^[0-9]+\$'"
