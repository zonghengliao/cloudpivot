\pset pager off
\echo '===== ① 未登记的 biz_type 流水（dev 现状） ====='
SELECT t.biz_type, t.doc_type, t.txn_type, t.doc_id, t.doc_no, t.qty
  FROM biz_inventory_txn t
 WHERE t.biz_type NOT IN (
       'PURCHASE_IN','PURCHASE_IN_CANCEL','OPENING','STOCK_TAKE_GAIN','STOCK_TAKE_LOSS',
       'STOCK_TAKE_CANCEL','TRANSFER_IN','TRANSFER_OUT','SERIAL_IN','SERIAL_OUT','SERIAL_SCRAP',
       'STOCK_OUT_CANCEL','ISSUE_OUT','SCRAP_OUT','LOSS_OUT','OTHER_OUT','PURCHASE_RETURN',
       'SALE_OUT','PRODUCTION_IN','PRODUCE_CANCEL','ADJUST_IN','ADJUST_OUT','PURCHASE')
 ORDER BY t.biz_type;

\echo '===== ② 该批流水是否已在 E4 的 DOC 轴上被 biz_stock_in 解释 ====='
SELECT t.biz_type, t.doc_no AS txn_doc_no, s.in_no, s.status AS stock_in_status,
       s.in_type, s.source_type, s.deleted_at IS NULL AS stock_in_alive,
       i.qty AS item_qty
  FROM biz_inventory_txn t
  LEFT JOIN biz_stock_in s ON s.id = t.doc_id
  LEFT JOIN biz_stock_in_item i ON i.stock_in_id = s.id
 WHERE t.biz_type = 'SALES_RETURN_IN';

\echo '===== ③ E4 posted CTE 口径是否含退货入库单（不过滤 in_type） ====='
SELECT count(*) AS posted_stock_in_rows,
       count(*) FILTER (WHERE s.in_type = 'RETURN') AS posted_return_rows
  FROM biz_stock_in s
 WHERE s.deleted_at IS NULL AND s.status = 'POSTED';
