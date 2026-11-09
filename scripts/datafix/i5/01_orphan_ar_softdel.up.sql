-- I-5 · 孤儿应收软删（145 cloudpivot_dev · 只改 5 张、只改 deleted_at）
-- 判据：source_type='SALES_OUT' 且 source_id 指向的 biz_stock_out 物理不存在（或已软删）
-- 为什么软删：保留审计痕迹，单据号/金额仍可回溯（物理删会丢证据）
-- 幂等：第二遍执行 ROW_COUNT=0 照常通过；行数不是 0 或 5 就整单回滚（防止夹具变动时误删）
\pset pager off
\echo '===== 处置前：真孤儿 5 张的快照 ====='
SELECT id, ar_no, source_no, source_id, amount, status, period_code, created_at
FROM biz_receivable
WHERE deleted_at IS NULL AND source_type = 'SALES_OUT' AND source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = biz_receivable.source_id AND s.deleted_at IS NULL)
ORDER BY ar_no;

\echo '===== 执行软删（5 个固定 id 白名单 + 判据双保险）====='
DO $$
DECLARE n int;
BEGIN
  UPDATE biz_receivable SET deleted_at = now(), updated_at = now()
   WHERE deleted_at IS NULL
     AND source_type = 'SALES_OUT'
     AND source_id IS NOT NULL
     AND NOT EXISTS (SELECT 1 FROM biz_stock_out s
                      WHERE s.id = biz_receivable.source_id AND s.deleted_at IS NULL)
     AND id IN (
       '6b0826a1-9fcd-4207-b2ce-c92a8f7b2d06',
       '5d4baf5e-b0e4-4be2-8c59-09c037aa2d4c',
       '29111b73-c787-4898-92b8-ddcdd98d3d20',
       'f5243e59-33dc-44bd-83c4-6bee786ca155',
       '92f715e1-c98b-4e0a-97f9-efd094b8e52a'
     );
  GET DIAGNOSTICS n = ROW_COUNT;
  RAISE NOTICE 'soft-deleted rows = %', n;
  IF n NOT IN (0, 5) THEN
    RAISE EXCEPTION 'unexpected row count %: 孤儿集合已变化，停止处置，先重新盘点', n;
  END IF;
END $$;

\echo '===== 处置后：真孤儿 ====='
SELECT count(*) AS orphan_sales_out
FROM biz_receivable r
WHERE r.deleted_at IS NULL AND r.source_type = 'SALES_OUT' AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL);

\echo '===== 处置后：卡片 SQL 口径（MANUAL 占位 source_id 仍会带出 16 行，属口径缺陷不是数据问题）====='
SELECT count(*) AS rows_from_card_sql
FROM biz_receivable r
WHERE r.deleted_at IS NULL AND r.source_id IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id = r.source_id AND s.deleted_at IS NULL);

\echo '===== 处置后：5 张已软删（deleted_at 非空，记录仍在）====='
SELECT ar_no, amount, status, deleted_at IS NOT NULL AS soft_deleted, updated_at
FROM biz_receivable
WHERE id IN ('6b0826a1-9fcd-4207-b2ce-c92a8f7b2d06','5d4baf5e-b0e4-4be2-8c59-09c037aa2d4c',
             '29111b73-c787-4898-92b8-ddcdd98d3d20','f5243e59-33dc-44bd-83c4-6bee786ca155',
             '92f715e1-c98b-4e0a-97f9-efd094b8e52a')
ORDER BY ar_no;

\echo '===== 处置后：MANUAL 16 张一行未动 ====='
SELECT count(*) AS manual_live, sum(amount) AS manual_amount
FROM biz_receivable WHERE deleted_at IS NULL AND source_type = 'MANUAL';
