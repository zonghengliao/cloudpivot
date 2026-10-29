-- ======================================================
-- V0081：公告索引（6 个业务索引 + 1 个公告编号唯一索引）
-- 依据：docs/批次C-技术架构.md §6 附录 A.2（**已在 PG 18.6 实跑通过**）
-- 回滚：V0081__announcement_indexes.down.sql
--
-- ── 为什么索引单独一个版本（不随 V0080 建表一起）──────────────
-- 与批次数 A / B 同口径（V0044 / V0064）：表结构与访问路径分版本，
-- 后续只调索引时不必回滚建表语句。
--
-- ── 三个部分索引各自服务一条确定的查询 ──────────────────────
--   idx_sys_announcement_list        我的可见列表（LIMIT 提前停，排除软删行）
--   idx_sys_announcement_expire      announcement_auto_expire 定时下线（只扫有到期的已发布行）
--   idx_sys_announcement_top_expire  announcement_top_expire 置顶到期
--
-- ── idx_sys_announcement_scope_reverse 是本卡验收的硬指标 ────
-- 可见性判定是「scope_type + target_id → announcement_id」（反向查找），
-- 索引列序必须与判定顺序一致：先等值列 scope_type，再 IN 列 target_id，
-- 最后带上投影列 announcement_id 以避免回表。POC-8 实测：219 行命中 0.016 ms。
--
-- ── 为什么不用 CONCURRENTLY ─────────────────────────────────
-- 新表（V0080 刚建）尚无并发读写；CONCURRENTLY 需要 `-- +no-transaction` 且会失去
-- 整文件回滚能力，对空表没有收益。
-- ======================================================

CREATE UNIQUE INDEX uk_sys_announcement_no ON sys_announcement (ann_no);
-- 列表主查询（status + 置顶 + 时间），部分索引排除软删
CREATE INDEX idx_sys_announcement_list ON sys_announcement (status, is_top DESC, publish_at DESC)
    WHERE deleted_at IS NULL;
-- 自动下线扫描（只扫有到期时间的已发布行）
CREATE INDEX idx_sys_announcement_expire ON sys_announcement (expire_at)
    WHERE status = 'PUBLISHED' AND expire_at IS NOT NULL;
-- 置顶到期扫描
CREATE INDEX idx_sys_announcement_top_expire ON sys_announcement (top_expire_at)
    WHERE is_top = TRUE AND top_expire_at IS NOT NULL;
-- 可见性判定的反向索引（scope_type + target_id → announcement_id）
CREATE INDEX idx_sys_announcement_scope_reverse ON sys_announcement_scope (scope_type, target_id, announcement_id);
-- 我的已读（uk 是 (announcement_id,user_id)，这里需要 user_id 打头）
CREATE INDEX idx_sys_announcement_read_user ON sys_announcement_read (user_id, announcement_id);
-- 附件孤儿巡检
CREATE INDEX idx_sys_announcement_attachment_file ON sys_announcement_attachment (file_id);
