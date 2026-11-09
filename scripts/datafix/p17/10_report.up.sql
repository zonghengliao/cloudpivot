-- P17 只读盘点：E2E-08 夹具清理漏删的两类残留（t_45bda7ad）
--   ① 孤儿库位账：material_id / warehouse_id 在 biz_material / biz_warehouse 里已不存在
--   ② 孤儿「无来源入库」凭证：source_type='SERIAL_BATCH' 且它的批次流水（txn.doc_id = voucher.source_id）已不存在
-- 只读，不改数据。判据全部是**形态判据**，不写死物料编码/凭证号。
\set ON_ERROR_STOP 1
\pset pager off

\echo '=== ① 孤儿库位账（主数据已硬删、账行留下）==='
SELECT l.id, l.material_id, l.warehouse_id, l.qty, l.created_at,
       (m.id IS NULL) AS material_missing,
       (w.id IS NULL) AS warehouse_missing
  FROM biz_inventory_location l
  LEFT JOIN biz_material  m ON m.id = l.material_id
  LEFT JOIN biz_warehouse w ON w.id = l.warehouse_id
 WHERE m.id IS NULL OR w.id IS NULL
 ORDER BY l.created_at;

\echo '=== ① 汇总 ==='
SELECT count(*) AS orphan_location_rows,
       coalesce(sum(qty), 0) AS orphan_location_qty
  FROM biz_inventory_location l
 WHERE NOT EXISTS (SELECT 1 FROM biz_material  m WHERE m.id = l.material_id)
    OR NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = l.warehouse_id);

\echo '=== ② 孤儿 SERIAL_BATCH 凭证（批次流水已不存在）==='
SELECT v.voucher_no, v.source_no, v.total_debit, v.status, v.created_at,
       (SELECT count(*) FROM biz_voucher_entry e WHERE e.voucher_id = v.id) AS entry_rows,
       (SELECT count(*) FROM biz_status_log  s WHERE s.entity_type = 'VOUCHER' AND s.entity_id = v.id) AS log_rows
  FROM biz_voucher v
 WHERE v.source_type = 'SERIAL_BATCH'
   AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = v.source_id)
 ORDER BY v.created_at;

\echo '=== ② 汇总 ==='
SELECT count(*) AS orphan_batch_vouchers,
       coalesce(sum(total_debit), 0) AS orphan_batch_amount
  FROM biz_voucher v
 WHERE v.source_type = 'SERIAL_BATCH'
   AND NOT EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = v.source_id);

\echo '=== ③ 仍存活的 SERIAL_BATCH 批次（有流水 ⇒ 不许动）==='
SELECT v.voucher_no, v.source_no, v.total_debit, v.created_at,
       (SELECT count(*) FROM biz_inventory_txn t WHERE t.doc_id = v.source_id) AS txn_rows
  FROM biz_voucher v
 WHERE v.source_type = 'SERIAL_BATCH'
   AND EXISTS (SELECT 1 FROM biz_inventory_txn t WHERE t.doc_id = v.source_id)
 ORDER BY v.created_at;
