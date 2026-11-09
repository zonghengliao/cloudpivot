\pset pager off
SELECT table_name, string_agg(column_name, ',' ORDER BY ordinal_position) AS cols
FROM information_schema.columns
WHERE table_name IN ('biz_receivable','biz_stock_out','biz_inventory','biz_inventory_txn','biz_inventory_location','biz_voucher','biz_voucher_entry')
GROUP BY table_name ORDER BY table_name;
