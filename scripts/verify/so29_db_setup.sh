#!/usr/bin/env bash
# =============================================================================
# SO-29 隔离库：建一个**空库** cloudpivot_so29（迁移由实例启动时全量应用）
# =============================================================================
#   bash scripts/verify/so29_db_setup.sh          # 先删后建（干净库）
#
# 为什么用空库而不是克隆邻居：本卡的验收要证明「全部迁移（含 V0303 建表 /
# V0304 权限码）在空库上能从零重放」，克隆库里迁移已应用就看不到 APPLY_OK。
#
# 表 owner 必须是应用账号 alun（`role "cloudpivot" 在 99 上不存在`，见
# skill references/isolated-db-and-env-traps.md）。
# =============================================================================
set -uo pipefail

DB=${SO29_DB:-cloudpivot_so29}
OWNER=${OWNER:-alun}

echo "==> 建库 $DB（owner=$OWNER）"
su - postgres -c "dropdb --if-exists $DB" || { echo "FATAL: dropdb 失败"; exit 1; }
su - postgres -c "createdb -O $OWNER $DB" || { echo "FATAL: createdb 失败"; exit 1; }

echo "==> 空库确认"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -tAc \
  "select current_database() || ' tables=' || count(*) from pg_tables where schemaname='public'" 
