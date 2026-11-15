-- ============================================================================
-- V0358 回滚：把 E2E06-FIX-3 回填掉的「超支」标记按快照还原
--
-- 还原条件（保守）：目标行**仍然是回填后的样子**才还原 ——
--   · 占用行：`over_budget = 0 AND over_amount IS NULL`
--   · 报销单：`over_budget = 0 AND missing_budget = 1 AND over_amount IS NULL`
-- 回填之后又被正常业务改过的行（例如撤回重提拿到 budget_id 后真超支）不碰，
-- 否则回滚会把「后来才成立的事实」抹掉。
--
-- 还原完删掉本 fix_code 的快照（保留表本身：其他数据修复还会往里写）。
-- 幂等：快照不存在时 UPDATE 命中 0 行。
-- ============================================================================

UPDATE biz_budget_usage u
   SET over_budget = (b.row_data ->> 'over_budget')::smallint,
       over_amount = (b.row_data ->> 'over_amount')::numeric,
       updated_at  = now()
  FROM biz_data_fix_backup b
 WHERE b.fix_code = 'E2E06-FIX-3'
   AND b.table_name = 'biz_budget_usage'
   AND b.row_id = u.id
   AND u.over_budget = 0
   AND u.over_amount IS NULL;

UPDATE biz_expense_report r
   SET over_budget    = (b.row_data ->> 'over_budget')::smallint,
       over_amount    = (b.row_data ->> 'over_amount')::numeric,
       missing_budget = (b.row_data ->> 'missing_budget')::smallint,
       updated_at     = now()
  FROM biz_data_fix_backup b
 WHERE b.fix_code = 'E2E06-FIX-3'
   AND b.table_name = 'biz_expense_report'
   AND b.row_id = r.id
   AND r.over_budget = 0
   AND r.missing_budget = 1
   AND r.over_amount IS NULL;

DELETE FROM biz_data_fix_backup WHERE fix_code = 'E2E06-FIX-3';
