-- V0102 回滚：移除 `sys_user.user_type`（先删约束再删列，避免残留约束名）
ALTER TABLE sys_user DROP CONSTRAINT IF EXISTS ck_sys_user_user_type;
ALTER TABLE sys_user DROP COLUMN IF EXISTS user_type;
