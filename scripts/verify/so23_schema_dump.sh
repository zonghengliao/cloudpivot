#!/usr/bin/env bash
# SO-23：打印造夹具要用的表结构（NOT NULL / 无默认值的列）+ 现成主数据 id
export PGPASSWORD=alun
DB=${1:-cloudpivot_so23fresh}
H="-h 127.0.0.1 -U alun -d $DB"

echo "=== 非空且无默认值的列（插入时必须显式给）"
psql $H -tAF'|' -c "
select table_name, string_agg(column_name, ',' order by ordinal_position)
  from information_schema.columns
 where table_schema='public'
   and table_name in ('biz_shipment','biz_shipment_item','biz_stock_out','biz_stock_out_item',
                      'biz_inventory','biz_inventory_location','biz_inventory_txn',
                      'biz_receivable','biz_receipt','biz_receipt_allocation','biz_invoice',
                      'biz_voucher','biz_voucher_entry','biz_sales_order','biz_sales_order_item')
   and is_nullable='NO' and column_default is null
 group by 1 order by 1"

echo "=== 主数据（各取 3 个）"
psql $H -tAF'|' -c "select 'material', id, material_code from biz_material where deleted_at is null limit 3"
psql $H -tAF'|' -c "select 'warehouse', id, warehouse_code from biz_warehouse where deleted_at is null limit 3"
psql $H -tAF'|' -c "select 'location', id, warehouse_id, location_code from biz_location where deleted_at is null limit 3"
psql $H -tAF'|' -c "select 'customer', id, customer_code from biz_customer where deleted_at is null limit 3"
psql $H -tAF'|' -c "select 'account', id, account_code, account_name from biz_account where account_code in ('1122','2221.02','5001','6401','1405') order by account_code"
psql $H -tAF'|' -c "select 'period', period_code, status from biz_period order by period_code desc limit 3"
psql $H -tAF'|' -c "select 'voucher_type_cols', column_name from information_schema.columns where table_name='biz_voucher' order by ordinal_position"
