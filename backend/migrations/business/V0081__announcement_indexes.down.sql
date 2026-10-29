-- ======================================================
-- V0081 回滚：DROP 公告 7 个索引（与 up 逆序）
-- ======================================================

DROP INDEX IF EXISTS idx_sys_announcement_attachment_file;
DROP INDEX IF EXISTS idx_sys_announcement_read_user;
DROP INDEX IF EXISTS idx_sys_announcement_scope_reverse;
DROP INDEX IF EXISTS idx_sys_announcement_top_expire;
DROP INDEX IF EXISTS idx_sys_announcement_expire;
DROP INDEX IF EXISTS idx_sys_announcement_list;
DROP INDEX IF EXISTS uk_sys_announcement_no;
