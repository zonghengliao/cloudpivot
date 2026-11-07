#!/usr/bin/env bash
# ============================================================================
# FIX-7（t_6207df79）145 取证 · 步骤 1：拉代码 / 建二进制副本 / 建**拷贝库** + 迁移前快照
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix7-inventory-biz-type/10_prepare_145.sh'
#
# 为什么用 cloudpivot_dev 的拷贝而不是空库：
#   · 拷贝库里有**真实的存量脏数据**（2 张销售出库单 + 4 条 OTHER_OUT 流水 + 2 张无源入库单），
#     正好是 V0330 的两类回填对象 —— 空库只能证明「新代码写新值」，证不了「存量回填」；
#   · 同时天然验证「存量库升级」路径（启动时把 V0330 应用到有数据的老库上）。
# 8033 服役实例不动（它连的还是 cloudpivot_dev，本卡一个字节都不改它）。
# ============================================================================
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH

cd /opt/cloudpivot
git fetch -q origin main
git pull --ff-only origin main
echo "HEAD=$(git rev-parse --short HEAD) $(git log -1 --format=%s)"

cd /opt/cloudpivot/backend
cargo build --bin cloudpivot
install -m 0755 target/debug/cloudpivot target/debug/cloudpivot-fix7
echo "binary_ok=$(stat -c %s target/debug/cloudpivot-fix7) bytes"
cd /opt/cloudpivot

export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
mkdir -p /root/backup
TS=$(date +%Y%m%d-%H%M%S)

# 迁移前备份（卡面要求：迁移前备份 145 测试库）
DUMP=/root/backup/cloudpivot_dev_FIX7_${TS}.dump
pg_dump -h 127.0.0.1 -U cloudpivot -Fc -f "$DUMP" cloudpivot_dev
echo "backup=$DUMP ($(du -h "$DUMP" | cut -f1))"

DB="cloudpivot_fix7_${TS//-/}"
psql -h 127.0.0.1 -U cloudpivot -d postgres -c "CREATE DATABASE $DB OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\""
pg_restore -h 127.0.0.1 -U cloudpivot -d "$DB" --no-owner --no-privileges "$DUMP"

echo "$DB" > /root/backup/.fix7_db
echo "db=$DB"
echo "迁移前 schema 版本：$(psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc "select max(version::int) from schema_migrations")"

EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$EV/raw"
# 迁移前快照：V0330 的两类回填对象 + 无源单现状（E2E 用它证明「存量被迁、且只迁该迁的」）
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -tAF'|' -c "
  select 'sales_out_docs', count(*) from biz_stock_out where source_type = 'SALES'
  union all
  select 'sales_txn_other_out', count(*) from biz_inventory_txn t
    join biz_stock_out s on s.id = t.doc_id
   where t.doc_type = 'STOCK_OUT' and s.source_type = 'SALES' and t.biz_type = 'OTHER_OUT'
  union all
  select 'sales_txn_sale_out', count(*) from biz_inventory_txn t
    join biz_stock_out s on s.id = t.doc_id
   where t.doc_type = 'STOCK_OUT' and s.source_type = 'SALES' and t.biz_type = 'SALE_OUT'
  union all
  select 'manual_other_out_txn', count(*) from biz_inventory_txn t
    join biz_stock_out s on s.id = t.doc_id
   where t.doc_type = 'STOCK_OUT' and coalesce(s.source_type,'') <> 'SALES' and t.biz_type = 'OTHER_OUT'
  union all
  select 'sourceless_docs', count(*) from biz_stock_in where is_sourceless
  union all
  select 'sourceless_purchase', count(*) from biz_stock_in where is_sourceless and in_type = 'PURCHASE'
  union all
  select 'inventory_rows', count(*) from biz_inventory
  union all
  select 'txn_rows', count(*) from biz_inventory_txn
  union all
  select 'reconcile_qty_diff', count(*) from (
    with inv as (select material_id, warehouse_id, sum(qty) q from biz_inventory group by 1,2),
         txn as (select material_id, warehouse_id,
                        sum(case when txn_type='IN' then qty else -qty end) q
                   from biz_inventory_txn group by 1,2)
    select 1 from inv full join txn using (material_id, warehouse_id)
     where coalesce(inv.q,0) <> coalesce(txn.q,0)) d" > "$EV/raw/pre_migration_state.txt"
echo "迁移前快照："
cat "$EV/raw/pre_migration_state.txt"
