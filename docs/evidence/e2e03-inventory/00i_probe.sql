SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name='biz_inventory_txn';
\echo '--- 流水 biz_type 分布 ---'
SELECT biz_type, doc_type, txn_type, count(*) FROM biz_inventory_txn GROUP BY 1,2,3 ORDER BY 1,2;
\echo '--- 销售出库单与流水对齐（FIX-7 回归）---'
SELECT o.out_no, o.out_type, o.source_type, o.status, t.biz_type, t.txn_type, t.qty
  FROM biz_stock_out o
  LEFT JOIN biz_inventory_txn t ON t.doc_id = o.id
 WHERE o.source_type = 'SALES' OR o.out_type = 'OTHER'
 ORDER BY o.created_at DESC LIMIT 20;
