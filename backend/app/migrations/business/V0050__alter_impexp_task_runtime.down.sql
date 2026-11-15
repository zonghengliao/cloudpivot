-- V0050 回滚：删除导出/导入任务的运行期列与索引。
--
-- 只回滚「本迁移新增的列」，V0041 建的表与既有索引不动；
-- 任务数据行保留（回滚只丢运行期元数据，业务数据不丢）。

DROP INDEX IF EXISTS idx_sys_export_task_due;
DROP INDEX IF EXISTS idx_sys_export_task_heartbeat;

ALTER TABLE sys_export_task
    DROP COLUMN IF EXISTS file_path,
    DROP COLUMN IF EXISTS file_sha256,
    DROP COLUMN IF EXISTS attempt_count,
    DROP COLUMN IF EXISTS max_attempts,
    DROP COLUMN IF EXISTS next_run_at,
    DROP COLUMN IF EXISTS cancel_requested,
    DROP COLUMN IF EXISTS locked_by,
    DROP COLUMN IF EXISTS heartbeat_at;

ALTER TABLE sys_import_task
    DROP COLUMN IF EXISTS attempt_count,
    DROP COLUMN IF EXISTS cancel_requested;

-- idx_sys_export_task_expire 由 V0041 建，回滚本迁移时不删。
