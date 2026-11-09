SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name='biz_stock_transfer';
\echo '--- 调拨凭证 ---'
SELECT v.id, v.voucher_no, v.source_type, v.source_id, v.total_debit, v.total_credit
  FROM biz_voucher v WHERE v.source_type IN ('STOCK_TRANSFER','TRANSFER') ORDER BY v.created_at DESC LIMIT 4;
\echo '--- 调拨单 ---'
SELECT transfer_no, status, from_warehouse_id, to_warehouse_id, remark FROM biz_stock_transfer WHERE remark LIKE 'E2E-03%' ORDER BY created_at DESC LIMIT 3;
