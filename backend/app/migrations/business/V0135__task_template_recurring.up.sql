-- V0135：周期任务（E11 · 二、周期任务）
--
-- 卡面口径：
--   * 给 `biz_task_template` 加 `cron_expr` / `enabled` / `next_run_at` / `last_run_at`；
--   * 调度复用 `Scheduler::register_builtin`，每 5 分钟扫一次 `next_run_at <= now()`；
--   * 生成的任务 `source='RECURRING'`。
--
-- 为什么要扩 `biz_task_source_check`：
--   V0100 把 `source` 值域钉死成 SELF / ASSIGNED / TRANSFERRED，周期任务要写 RECURRING，
--   不扩 CHECK 会在 INSERT 时 500（`biz_task_source_check` 违反）。
--   PostgreSQL 不支持往既有 CHECK 追加取值 → 必须 DROP + ADD。

ALTER TABLE biz_task_template
    ADD COLUMN cron_expr   VARCHAR(100), -- 标准 cron（5 段：分 时 日 月 周；6 段时首段是秒，被忽略）
    ADD COLUMN enabled     BOOLEAN NOT NULL DEFAULT FALSE, -- 周期开关（关掉即不再生成）
    ADD COLUMN next_run_at TIMESTAMPTZ, -- 下次生成时间（UTC；空 = 未排期）
    ADD COLUMN last_run_at TIMESTAMPTZ; -- 上次生成时间（UTC）

COMMENT ON COLUMN biz_task_template.cron_expr IS '标准 cron 表达式（5 段 = 分 时 日 月 周；6 段时首段为秒并被忽略）';
COMMENT ON COLUMN biz_task_template.enabled IS '周期开关：true 且 cron_expr 非空时，调度器才会按 next_run_at 生成任务';
COMMENT ON COLUMN biz_task_template.next_run_at IS '下次生成时间（UTC）：到点后由每 5 分钟的调度扫描拾取，生成成功即推进';
COMMENT ON COLUMN biz_task_template.last_run_at IS '上次生成时间（UTC）';

-- 到点扫描的部分索引（只覆盖启用 + 未删且已排期的模板）
CREATE INDEX idx_biz_task_template_due
    ON biz_task_template (next_run_at)
 WHERE deleted_at IS NULL AND enabled AND next_run_at IS NOT NULL;

-- `source` 值域扩 RECURRING
ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS biz_task_source_check;
ALTER TABLE biz_task
    ADD CONSTRAINT biz_task_source_check
    CHECK (source IN ('SELF', 'ASSIGNED', 'TRANSFERRED', 'RECURRING'));

COMMENT ON COLUMN biz_task.source IS '来源：SELF 自建 / ASSIGNED 被分派 / TRANSFERRED 被转派 / RECURRING 周期任务生成';

-- ---------- 自检 ----------
DO $$
DECLARE
    n_col integer;
    v_def text;
BEGIN
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_task_template'
       AND column_name IN ('cron_expr', 'enabled', 'next_run_at', 'last_run_at');
    IF n_col <> 4 THEN
        RAISE EXCEPTION 'V0135 自检失败：周期相关列只建成 % 列（应为 4）', n_col;
    END IF;

    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conrelid = 'biz_task'::regclass AND conname = 'biz_task_source_check';
    IF v_def IS NULL OR position('RECURRING' in v_def) = 0 THEN
        RAISE EXCEPTION 'V0135 自检失败：biz_task_source_check 未包含 RECURRING（实际 %）', v_def;
    END IF;

    RAISE NOTICE 'V0135 自检通过：周期列 4 个 + source 值域含 RECURRING';
END $$;
