#!/usr/bin/env bash
# =============================================================================
# SO-30 隔离库：建一个**空库** cloudpivot_so30（迁移由实例启动时全量应用）
# =============================================================================
#   bash scripts/verify/so30_db_setup.sh          # 先删后建（干净库）
#
# 与 so29_db_setup.sh 同口径：空库 + 启动全量重放，顺带证明 V0305（来源两列 +
# 发票来源白名单 + 部分唯一索引）在零数据上能落。
# owner 必须是应用账号 alun（role "cloudpivot" 在 99 上不存在，见
# skill references/isolated-db-and-env-traps.md）。
# =============================================================================
set -uo pipefail

DB=${SO30_DB:-cloudpivot_so30}
OWNER=${OWNER:-alun}

echo "==> 建库 $DB（owner=$OWNER）"
su - postgres -c "dropdb --if-exists $DB" || { echo "FATAL: dropdb 失败"; exit 1; }
su - postgres -c "createdb -O $OWNER $DB" || { echo "FATAL: createdb 失败"; exit 1; }

echo "==> 空库确认"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -tAc \
  "select current_database() || ' tables=' || count(*) from pg_tables where schemaname='public'"
