\pset pager off
\echo '===== 口径对比：孤儿应收（UNION 口径 vs 带 source_type vs 带 deleted_at） ====='
WITH scoped AS (
  SELECT r.ar_no, r.source_type, r.source_no, r.source_id, r.deleted_at
    FROM biz_receivable r
   WHERE r.source_id IS NOT NULL
)
SELECT '① 卡面口径（不加 source_type、含软删行）' AS 口径, count(*) AS 行数
  FROM scoped s
 WHERE NOT EXISTS (SELECT 1 FROM biz_stock_out o WHERE o.id = s.source_id)
UNION ALL
SELECT '② 只加 source_type=SALES_OUT（仍含软删行）', count(*)
  FROM scoped s
 WHERE s.source_type = 'SALES_OUT'
   AND NOT EXISTS (SELECT 1 FROM biz_stock_out o WHERE o.id = s.source_id)
UNION ALL
SELECT '③ 巡检口径（source_type=SALES_OUT + 应收未删 + 出库单未删）', count(*)
  FROM scoped s
 WHERE s.source_type = 'SALES_OUT' AND s.deleted_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM biz_stock_out o WHERE o.id = s.source_id AND o.deleted_at IS NULL)
UNION ALL
SELECT '④ 巡检现状（source_type=SALES_OUT + 应收未删 + 出库单不看 deleted_at）', count(*)
  FROM scoped s
 WHERE s.source_type = 'SALES_OUT' AND s.deleted_at IS NULL
   AND NOT EXISTS (SELECT 1 FROM biz_stock_out o WHERE o.id = s.source_id);

\echo '===== ① 口径里被误算的行（按 source_type 分组） ====='
SELECT source_type, source_no, count(*) AS 行数
  FROM biz_receivable r
 WHERE r.source_id IS NOT NULL
   AND NOT EXISTS (SELECT 1 FROM biz_stock_out o WHERE o.id = r.source_id)
 GROUP BY 1, 2 ORDER BY 1, 2;
