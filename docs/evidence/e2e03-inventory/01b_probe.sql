SELECT string_agg(column_name||':'||data_type, ', ' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name='biz_voucher';
SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name='biz_stock_in_item';
SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name='biz_stock_in_item_serial';
\echo '--- MS 相关凭证 ---'
SELECT v.id, v.voucher_no, v.voucher_date, v.source_type, v.source_id, v.status
  FROM biz_voucher v WHERE v.voucher_date >= '2026-11-09' ORDER BY v.created_at DESC LIMIT 8;
