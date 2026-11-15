-- 回滚 V0365：去掉 `biz_status_log.action`（业务动作码）。
--
-- 只删「本迁移加的」东西；`biz_status_log` 其它列与存量行一概不动。
DROP INDEX IF EXISTS idx_status_log_action;
ALTER TABLE biz_status_log DROP COLUMN IF EXISTS action;
