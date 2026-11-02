\pset pager off
\echo '=== 0) 库与版本'
select current_database() db, version() pg;
\echo '=== 1) 列清单（PO / SI / INV / PAYABLE / SUPPLIER）'
select table_name, count(*) cols from information_schema.columns
 where table_schema='public' and table_name in ('biz_purchase_order','biz_stock_in','biz_inventory','biz_payable','biz_supplier')
 group by 1 order by 1;
\echo '=== 1b) biz_purchase_order 列'
select ordinal_position n, column_name, data_type, is_nullable, coalesce(column_default,'') dflt
 from information_schema.columns where table_schema='public' and table_name='biz_purchase_order' order by ordinal_position;
\echo '=== 1c) biz_stock_in 列'
select ordinal_position n, column_name, data_type, is_nullable, coalesce(column_default,'') dflt
 from information_schema.columns where table_schema='public' and table_name='biz_stock_in' order by ordinal_position;
\echo '=== 1d) biz_inventory 列'
select ordinal_position n, column_name, data_type, is_nullable, coalesce(column_default,'') dflt
 from information_schema.columns where table_schema='public' and table_name='biz_inventory' order by ordinal_position;
\echo '=== 2) CHECK 约束（四表全量）'
select rel.relname tbl, con.conname, pg_get_constraintdef(con.oid) def
 from pg_constraint con join pg_class rel on rel.oid=con.conrelid join pg_namespace ns on ns.oid=rel.relnamespace
 where ns.nspname='public' and rel.relname in ('biz_purchase_order','biz_stock_in','biz_inventory','biz_payable','biz_supplier') and con.contype='c'
 order by 1,2;
\echo '=== 3) 索引'
select tablename, indexname, indexdef from pg_indexes
 where schemaname='public' and tablename in ('biz_purchase_order','biz_stock_in','biz_inventory','biz_payable','biz_supplier')
 order by 1,2;
\echo '=== 4) 外键计数（单据链）'
select rel.relname tbl, count(*) fks
 from pg_constraint con join pg_class rel on rel.oid=con.conrelid join pg_namespace ns on ns.oid=rel.relnamespace
 where ns.nspname='public' and con.contype='f' and rel.relname in ('biz_purchase_order','biz_stock_in','biz_payable','biz_voucher','biz_inventory','biz_contract' )
 group by 1 order by 1;
\echo '=== 5) 关键列存在性探针（巡检/DDL 引用的列）'
select 'biz_purchase_order' t, c, exists(select 1 from information_schema.columns x where x.table_schema='public' and x.table_name='biz_purchase_order' and x.column_name=c) ok from unnest(array['id','order_no','supplier_id','order_date','status','total_amount','total_with_tax','created_at','updated_at','deleted_at','contract_id','purchase_type','contract_no_snapshot','supplier_name_snapshot']) c
union all
select 'biz_stock_in', c, exists(select 1 from information_schema.columns x where x.table_schema='public' and x.table_name='biz_stock_in' and x.column_name=c) from unnest(array['id','in_no','in_type','source_order_id','supplier_id','warehouse_id','status','created_at','updated_at','deleted_at','contract_id','source_kind']) c
union all
select 'biz_payable', c, exists(select 1 from information_schema.columns x where x.table_schema='public' and x.table_name='biz_payable' and x.column_name=c) from unnest(array['id','payable_no','source_type','source_id','supplier_id','status','deleted_at','updated_at']) c
union all
select 'biz_supplier', c, exists(select 1 from information_schema.columns x where x.table_schema='public' and x.table_name='biz_supplier' and x.column_name=c) from unnest(array['id','supplier_no','supplier_name','status','deleted_at','updated_at']) c
union all
select 'biz_inventory_txn', c, exists(select 1 from information_schema.columns x where x.table_schema='public' and x.table_name='biz_inventory_txn' and x.column_name=c) from unnest(array['doc_type','doc_id','doc_no','biz_type']) c
union all
select 'biz_seq', c, exists(select 1 from information_schema.columns x where x.table_schema='public' and x.table_name='biz_seq' and x.column_name=c) from unnest(array['seq_type','prefix','date_pattern','period_key','current_no','step','separator','status','remark','deleted_at']) c
order by 1,2;
\echo '=== 6) 触发器数'
select count(*) triggers from information_schema.triggers where trigger_schema='public';
\echo '=== 7) 存量行数'
select 'biz_purchase_order' t, count(*) from biz_purchase_order
union all select 'biz_stock_in', count(*) from biz_stock_in
union all select 'biz_inventory', count(*) from biz_inventory
union all select 'biz_payable', count(*) from biz_payable
union all select 'biz_supplier', count(*) from biz_supplier;
\echo '=== 8) biz_stock_in in_type / status 实际取值分布'
select in_type, status, count(*) from biz_stock_in group by 1,2 order by 1,2;
\echo '=== 9) 入库单供应商 vs 其采购单（F1 存量漂移，实测）'
select count(*) rows_total,
       count(*) filter (where s.source_order_id is not null) with_source,
       count(*) filter (where s.source_order_id is not null and s.supplier_id is distinct from o.supplier_id) mismatch
  from biz_stock_in s left join biz_purchase_order o on o.id=s.source_order_id and o.deleted_at is null
 where s.deleted_at is null;
