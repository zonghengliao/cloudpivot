#!/usr/bin/env bash
# 找「销售链数据最全」的现有库，作为 SO-23 不变量的正向基线库选择依据
export PGPASSWORD=alun
for db in cloudpivot_so22 cloudpivot_so22e2e cloudpivot_so22fresh cloudpivot_so22fresh2 cloudpivot_so22t cloudpivot_so20 cloudpivot_so19 cloudpivot_so18 cloudpivot_so16 cloudpivot_so15 cloudpivot_so14 cloudpivot_so13 cloudpivot_so12; do
  out=$(psql -h 127.0.0.1 -U alun -d "$db" -tAF'|' -c "
    select (select count(*) from biz_sales_order),
           (select count(*) from biz_shipment),
           (select count(*) from biz_stock_out where source_type='SALES'),
           (select count(*) from biz_receivable),
           (select count(*) from biz_receipt),
           (select count(*) from biz_receipt_allocation),
           (select count(*) from biz_invoice),
           (select count(*) from biz_voucher),
           (select version from schema_migrations order by 1 desc limit 1)
  " 2>/dev/null)
  if [ -n "$out" ]; then
    echo "$db | SO=$(echo $out|cut -d'|' -f1) SH=$(echo $out|cut -d'|' -f2) OUT=$(echo $out|cut -d'|' -f3) AR=$(echo $out|cut -d'|' -f4) RC=$(echo $out|cut -d'|' -f5) ALLOC=$(echo $out|cut -d'|' -f6) IV=$(echo $out|cut -d'|' -f7) V=$(echo $out|cut -d'|' -f8) v=$(echo $out|cut -d'|' -f9)"
  fi
done
