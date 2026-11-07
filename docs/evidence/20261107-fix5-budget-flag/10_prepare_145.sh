#!/usr/bin/env bash
# ============================================================================
# FIX-5（t_9a23375a）145 取证 · 步骤 1：拉代码 / 建二进制副本 / 建**拷贝库**
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261107-fix5-budget-flag/10_prepare_145.sh'
#
# 为什么用 cloudpivot_dev 的拷贝而不是空库：
#   · 拷贝库有真实的部门 / 会计科目 / 已发布的 expense 审批流，E2E 才能走「真的提交审批」；
#   · 同时天然证明 **存量库升级** 路径：启动时会把 V0325 应用到有数据的老库上。
# 8033 的服役实例不动（它连的还是 cloudpivot_dev，本卡一个字节都不改它）。
# ============================================================================
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH

cd /opt/cloudpivot
git fetch -q origin main
git pull --ff-only origin main
echo "HEAD=$(git rev-parse --short HEAD) $(git log -1 --format=%s)"

cd /opt/cloudpivot/backend
cargo build --bin cloudpivot
install -m 0755 target/debug/cloudpivot target/debug/cloudpivot-fix5
echo "binary_ok=$(stat -c %s target/debug/cloudpivot-fix5) bytes"
cd /opt/cloudpivot

export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
mkdir -p /root/backup
TS=$(date +%Y%m%d-%H%M%S)

DUMP=/root/backup/cloudpivot_dev_FIX5_${TS}.dump
pg_dump -h 127.0.0.1 -U cloudpivot -Fc -f "$DUMP" cloudpivot_dev
echo "backup=$DUMP ($(du -h "$DUMP" | cut -f1))"

DB="cloudpivot_fix5_${TS//-/}"
psql -h 127.0.0.1 -U cloudpivot -d postgres -v ON_ERROR_STOP=0 -c "CREATE DATABASE $DB OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\""
pg_restore -h 127.0.0.1 -U cloudpivot -d "$DB" --no-owner --no-privileges "$DUMP"

echo "$DB" > /root/backup/.fix5_db
echo "db=$DB"
echo "迁移前 schema 版本：$(psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select max(version) from schema_migrations")"
