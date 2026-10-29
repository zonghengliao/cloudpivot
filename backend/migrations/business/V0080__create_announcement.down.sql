-- ======================================================
-- V0080 回滚：逆序 DROP 公告四张表
--
-- 附件 / 已读 / 范围三张子表都有 ON DELETE CASCADE 指向 sys_announcement，
-- 但**仍然**按逆序显式 DROP：CASCADE 只保证删父行时级联删子行，不保证删父表时
-- 不留下「表还在、外键约束指向已删表」的中间态（且 DROP TABLE 不带 CASCADE 时
-- 会直接报错）。显式逆序是最少意外的写法。
-- ======================================================

DROP TABLE IF EXISTS sys_announcement_attachment;
DROP TABLE IF EXISTS sys_announcement_read;
DROP TABLE IF EXISTS sys_announcement_scope;
DROP TABLE IF EXISTS sys_announcement;
