#!/usr/bin/env bash
# ============================================================================
# FIX-4（t_763677cd）145 取证 · 步骤 1：拉代码 / 建二进制副本 / 备份测试库 / 建隔离库
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261107-fix4-inventory-bucket/30_prepare_145.sh'
# 说明：
#   · 145 上 8033 的服役实例（cloudpivot-test）**不动**，本卡一律用隔离库 + 隔离端口 8045；
#   · 「迁移前先备份 145 测试库」在这里落地（pg_dump -Fc 到 /root/backup）。
# ============================================================================
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH

cd /opt/cloudpivot
git pull --ff-only origin main
echo "HEAD=$(git rev-parse --short HEAD) $(git log -1 --format=%s)"

cargo build --bin cloudpivot
install -m 0755 target/debug/cloudpivot target/debug/cloudpivot-fix4
echo "binary_ok=$(stat -c %s target/debug/cloudpivot-fix4) bytes"

export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
mkdir -p /root/backup
TS=$(date +%Y%m%d-%H%M%S)
DUMP=/root/backup/cloudpivot_dev_FIX4_${TS}.dump
pg_dump -h 127.0.0.1 -U cloudpivot -Fc -f "$DUMP" cloudpivot_dev
echo "backup=$DUMP ($(du -h "$DUMP" | cut -f1))"

# 隔离库（名字带时间戳：天然「全新库」，不需要 DROP 任何东西）
DB_NEW="cloudpivot_fix4n_${TS//-/}"
DB_LEGACY="cloudpivot_fix4legacy_${TS//-/}"
POSTGRES_PW=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)

psql -h 127.0.0.1 -U cloudpivot -d postgres -v ON_ERROR_STOP=0 -c "CREATE DATABASE $DB_NEW OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB_NEW OWNER cloudpivot\""
psql -h 127.0.0.1 -U cloudpivot -d postgres -v ON_ERROR_STOP=0 -c "CREATE DATABASE $DB_LEGACY OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB_LEGACY OWNER cloudpivot\""

# 存量迁移用的库：cloudpivot_dev 的**拷贝**（真数据，不动共享库）
pg_restore -h 127.0.0.1 -U cloudpivot -d "$DB_LEGACY" --no-owner --no-privileges "$DUMP"

echo "$DB_NEW" > /root/backup/.fix4_db_new
echo "$DB_LEGACY" > /root/backup/.fix4_db_legacy
echo "db_new=$DB_NEW"
echo "db_legacy=$DB_LEGACY"
