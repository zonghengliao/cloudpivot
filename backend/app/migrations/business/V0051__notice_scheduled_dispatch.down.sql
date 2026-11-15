-- 回滚 V0051：删除定时发送相关列 / 约束 / 索引（不动 V0043 的既有结构）。
DROP INDEX IF EXISTS idx_sys_notice_due;

ALTER TABLE sys_notice DROP CONSTRAINT IF EXISTS ck_sys_notice_dispatch_attempts;
ALTER TABLE sys_notice DROP CONSTRAINT IF EXISTS ck_sys_notice_dispatch_status;

ALTER TABLE sys_notice DROP COLUMN IF EXISTS dispatch_attempts;
ALTER TABLE sys_notice DROP COLUMN IF EXISTS dispatch_status;
ALTER TABLE sys_notice DROP COLUMN IF EXISTS scheduled_at;
