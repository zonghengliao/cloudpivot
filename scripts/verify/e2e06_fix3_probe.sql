-- ============================================================================
-- E2E06-FIX-3 验收探针（只读）
--
-- 跑法（任一带业务的库）：
--   psql "$DATABASE_URL" -P pager=off -At -F '|' -f scripts/verify/e2e06_fix3_probe.sql
-- 或 145：
--   set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
--   PGPASSWORD=$NIK_DATABASE_PASSWORD psql -h 127.0.0.1 -U $NIK_DATABASE_USER \
--     -d $NIK_DATABASE_NAME -P pager=off -At -F '|' -f scripts/verify/e2e06_fix3_probe.sql
--
-- 期望：
--   dirty_usage   = 0     （验收 ①：回填后「未编预算却标超支」的占用行为 0）
--   dirty_report  = 0     （同一口径的单据头）
--   true_over     >= 0    （验收 ②：有 budget_id 的真超支行不受影响，数量不因回填从 N 变 0）
--   backup_rows   >= 0    （验收 ③：回填前的原值快照）
--   cross_period  >= 0    （E2E06-FIX-3 ②：跨期单据样本，供人工核对两个期间）
-- ============================================================================

SELECT 'dirty_usage',
       count(*)::text
  FROM biz_budget_usage
 WHERE over_budget = 1 AND budget_id IS NULL AND deleted_at IS NULL
UNION ALL
SELECT 'dirty_report',
       count(*)::text
  FROM biz_expense_report r
 WHERE r.over_budget = 1
   AND r.deleted_at IS NULL
   AND EXISTS (SELECT 1 FROM biz_budget_usage u
                WHERE u.source_type = 'EXPENSE_REPORT' AND u.source_id = r.id
                  AND u.deleted_at IS NULL)
   AND NOT EXISTS (SELECT 1 FROM biz_budget_usage u
                    WHERE u.source_type = 'EXPENSE_REPORT' AND u.source_id = r.id
                      AND u.deleted_at IS NULL AND u.budget_id IS NOT NULL)
UNION ALL
SELECT 'true_over_usage',
       count(*)::text
  FROM biz_budget_usage
 WHERE over_budget = 1 AND budget_id IS NOT NULL AND deleted_at IS NULL
UNION ALL
SELECT 'true_over_report',
       count(*)::text
  FROM biz_expense_report
 WHERE over_budget = 1 AND deleted_at IS NULL
UNION ALL
SELECT 'backup_rows',
       count(*)::text
  FROM biz_data_fix_backup
 WHERE fix_code = 'E2E06-FIX-3'
UNION ALL
SELECT 'cross_period',
       count(*)::text
  FROM biz_expense_report r
 WHERE r.deleted_at IS NULL
   AND r.voucher_id IS NOT NULL
   AND to_char(r.expense_date, 'YYYY-MM') <> r.period_code;

-- 明细：跨期单据（费用归属期间 vs 记账期间）+ 其预算占用期间，人工核对「两个期间」都对
SELECT '--- 跨期样本',
       r.report_no || '|' || r.status
       || '|费用期间=' || to_char(r.expense_date, 'YYYY-MM')
       || '|记账期间=' || r.period_code
       || '|占用期间=' || coalesce((SELECT string_agg(DISTINCT u.period_code, ',')
                                      FROM biz_budget_usage u
                                     WHERE u.source_type = 'EXPENSE_REPORT'
                                       AND u.source_id = r.id AND u.deleted_at IS NULL), '-')
  FROM biz_expense_report r
 WHERE r.deleted_at IS NULL
   AND r.voucher_id IS NOT NULL
   AND to_char(r.expense_date, 'YYYY-MM') <> r.period_code
 ORDER BY r.approved_at DESC NULLS LAST
 LIMIT 10;

-- 明细：本次修复过的行（快照里的原值 vs 现在的值），审计用
SELECT '--- 已回填',
       b.table_name || '|' || coalesce(b.row_data ->> 'source_no', b.row_data ->> 'report_no', b.row_id::text)
       || '|改前 over_budget=' || (b.row_data ->> 'over_budget')
       || ' over_amount=' || coalesce(b.row_data ->> 'over_amount', 'NULL')
  FROM biz_data_fix_backup b
 WHERE b.fix_code = 'E2E06-FIX-3'
 ORDER BY b.table_name, b.backed_up_at
 LIMIT 20;
