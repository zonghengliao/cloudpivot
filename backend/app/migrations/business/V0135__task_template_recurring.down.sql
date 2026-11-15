-- V0135 down：撤销周期列与 RECURRING 来源
ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS biz_task_source_check;
ALTER TABLE biz_task
    ADD CONSTRAINT biz_task_source_check
    CHECK (source IN ('SELF', 'ASSIGNED', 'TRANSFERRED'));

-- 存量 RECURRING 行会挡住还原后的 CHECK → 先降级成 SELF（只改来源标记，不动任务本身）
UPDATE biz_task SET source = 'SELF' WHERE source = 'RECURRING';

DROP INDEX IF EXISTS idx_biz_task_template_due;

ALTER TABLE biz_task_template
    DROP COLUMN IF EXISTS last_run_at,
    DROP COLUMN IF EXISTS next_run_at,
    DROP COLUMN IF EXISTS enabled,
    DROP COLUMN IF EXISTS cron_expr;
