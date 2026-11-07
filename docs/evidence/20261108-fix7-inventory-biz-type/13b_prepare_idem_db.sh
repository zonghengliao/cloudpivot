#!/usr/bin/env bash
# ============================================================================
# FIX-7 145 取证 · 步骤 3b：幂等/空库首启专用干净拷贝库
#
# 为什么要另起一份拷贝库：
#   `12_run_db_tests.sh` 里 `stockout_tests` 的清场口径是
#   `DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-%'`（模块历史遗留的宽删），
#   跑完会把**存量销售出库单**一起删掉 —— 之后再做「down 是否把 SALE_OUT 还原成
#   OTHER_OUT」的往返验证就查不到那批流水（单据没了 join 不上，不是迁移的问题）。
#   所以幂等脚本用一份**没跑过单测**的干净拷贝库（同样从 cloudpivot_dev 的迁移前 dump 还原）。
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix7-inventory-biz-type/13b_prepare_idem_db.sh'
# ============================================================================
set -euo pipefail

DUMP=$(ls -t /root/backup/cloudpivot_dev_FIX7_*.dump | head -1)
DB="cloudpivot_fix7_idem_$(date +%Y%m%d%H%M%S)"
export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)

psql -h 127.0.0.1 -U cloudpivot -d postgres -c "CREATE DATABASE $DB OWNER cloudpivot" 2>/dev/null \
  || su - postgres -c "psql -v ON_ERROR_STOP=1 -c \"CREATE DATABASE $DB OWNER cloudpivot\""
pg_restore -h 127.0.0.1 -U cloudpivot -d "$DB" --no-owner --no-privileges "$DUMP"
echo "$DB" > /root/backup/.fix7_db
echo "dump=$DUMP"
echo "db=$DB"
echo "迁移前 schema：$(psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAc 'select max(version::int) from schema_migrations')"
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -tAF'|' -c "
  select 'sales_txn_other_out', count(*) from biz_inventory_txn t join biz_stock_out s on s.id=t.doc_id
   where t.doc_type='STOCK_OUT' and s.source_type='SALES' and t.biz_type='OTHER_OUT'
  union all
  select 'sourceless_purchase', count(*) from biz_stock_in where is_sourceless and in_type='PURCHASE'"
