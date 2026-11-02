-- Q1 补充量测：把「JOIN 的成本到底出在哪」量清楚（首屏 vs 深翻页 vs 全量导出）
\pset pager off

\echo '===== F. 现状（无 JOIN）深翻页 OFFSET 2000：与 D 对比看 JOIN 在深翻页里到底占多少 ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT id, order_no, supplier_id, order_date, status, total_with_tax
  FROM biz_purchase_order
 WHERE deleted_at IS NULL
 ORDER BY order_date DESC, order_no DESC
 LIMIT 20 OFFSET 2000;

\echo ''
\echo '===== G. 方案A：全量导出（无 LIMIT）10 万行 + JOIN 合同 + 供应商 ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT o.id, o.order_no, o.order_date, o.total_with_tax, c.contract_no, s.supplier_name
  FROM biz_purchase_order o
  LEFT JOIN biz_contract c ON c.id = o.contract_id AND c.deleted_at IS NULL
  LEFT JOIN biz_supplier s ON s.id = c.party_b_id  AND s.deleted_at IS NULL
 WHERE o.deleted_at IS NULL;

\echo ''
\echo '===== H. 方案B：全量导出（无 LIMIT）10 万行，同表快照 ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT o.id, o.order_no, o.order_date, o.total_with_tax, o.contract_no_snapshot, o.supplier_name_snapshot
  FROM biz_purchase_order o
 WHERE o.deleted_at IS NULL;

\echo ''
\echo '===== I. 方案A：按供应商名模糊筛（列表筛选框，必须 JOIN 才能按名称过滤） ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT o.id, o.order_no, o.order_date
  FROM biz_purchase_order o
  JOIN biz_contract c ON c.id = o.contract_id AND c.deleted_at IS NULL
  JOIN biz_supplier s ON s.id = c.party_b_id  AND s.deleted_at IS NULL
 WHERE o.deleted_at IS NULL AND s.supplier_name LIKE '%基准供应商 77%'
 LIMIT 20;

\echo ''
\echo '===== J. 方案B：按供应商名模糊筛（同表列，无需 JOIN） ====='
EXPLAIN (ANALYZE, BUFFERS OFF, SUMMARY ON)
SELECT o.id, o.order_no, o.order_date
  FROM biz_purchase_order o
 WHERE o.deleted_at IS NULL AND o.supplier_name_snapshot LIKE '%基准供应商 77%'
 LIMIT 20;
