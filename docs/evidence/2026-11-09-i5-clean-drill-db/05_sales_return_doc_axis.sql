\pset pager off
SELECT t.biz_type, t.doc_type, t.qty, t.doc_no,
       s.in_type, s.status AS stockin_status, s.source_type, s.deleted_at
FROM biz_inventory_txn t
LEFT JOIN biz_stock_in s ON s.id = t.doc_id
WHERE t.biz_type = 'SALES_RETURN_IN';

\echo '===== E4 DOC 轴是否已计入这批数量（biz_stock_in POSTED 明细）====='
SELECT s.in_no, s.status, s.source_type, i.material_id, i.qty
FROM biz_stock_in s JOIN biz_stock_in_item i ON i.stock_in_id = s.id
WHERE s.source_type = 'SALES_RETURN' AND s.deleted_at IS NULL
ORDER BY s.created_at DESC LIMIT 10;
