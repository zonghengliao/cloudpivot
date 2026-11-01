#!/usr/bin/env bash
# R5 生产（144）只读核对：迁移版本 / 期初节点 / 约束 / 索引 / 库存与期间基线
set -uo pipefail
cd /opt/cloudpivot
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
Q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -tAc "$1"; }

echo "max_migration=$(Q 'select max(version::int) from schema_migrations')"
echo "opening_nodes=$(Q "select count(*) from sys_menu where auth_code like 'business:erp:opening:%' and deleted_at is null")"
echo "opening_node_ids=$(Q "select string_agg(id::text, ',' order by id) from sys_menu where auth_code like 'business:erp:opening:%' and deleted_at is null")"
echo "voucher_type_ck=$(Q "select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_voucher_type'")"
echo "opening_uk=$(Q "select indexname from pg_indexes where indexname='uk_biz_voucher_opening_period'")"
echo "opening_balance_rows=$(Q 'select count(*) from biz_opening_balance')"
echo "opening_inventory_rows=$(Q 'select count(*) from biz_opening_inventory')"
echo "opening_vouchers=$(Q "select count(*) from biz_voucher where voucher_type='OPENING'")"
echo "periods=$(Q "select string_agg(period_code || ':' || status, ',' order by period_code) from biz_accounting_period")"
echo "inventory_rows=$(Q 'select count(*) from biz_inventory')"
echo "voucher_rows=$(Q 'select count(*) from biz_voucher')"
