\pset footer off
\echo '== A. 版本/表清单 =='
SELECT tablename FROM pg_tables WHERE schemaname='public' AND (tablename LIKE '%migration%' OR tablename LIKE '%version%') ORDER BY 1;
SELECT count(*) AS public_tables FROM pg_tables WHERE schemaname='public';
SELECT to_regclass('public.biz_supplier') AS supplier,
       to_regclass('public.biz_purchase_order') AS po,
       to_regclass('public.biz_contract') AS contract,
       to_regclass('public.biz_fixed_asset') AS fixed_asset,
       to_regclass('public.biz_flow_instance') AS flow_inst,
       to_regclass('public.biz_stock_in') AS stock_in,
       to_regclass('public.biz_payable') AS payable;
\echo '== B. 采购单列 =='
SELECT column_name FROM information_schema.columns
 WHERE table_schema='public' AND table_name='biz_purchase_order'
   AND column_name IN ('supplier_id','supplier_name_snapshot','purchase_type','contract_id','created_at','updated_at','deleted_at','status')
 ORDER BY 1;
\echo '== C. 供应商计数 =='
SELECT count(*) FILTER (WHERE deleted_at IS NULL) AS active,
       count(*) FILTER (WHERE deleted_at IS NOT NULL) AS soft_deleted,
       count(*) FILTER (WHERE deleted_at IS NULL AND coalesce(btrim(short_name),'') <> '') AS active_with_short
  FROM biz_supplier;
