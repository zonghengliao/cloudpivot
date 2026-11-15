-- ======================================================
-- V0049：消息通知索引（性能关键路径）
--
-- 依据：`docs/批次A-技术架构.md` §4.2 / §4.3 与 §7 附录 A（V0044/V0045 索引段原文）。
-- 三张通知表的表结构在 V0043，模板唯一索引随 V0044（种子依赖它）；本文件专放
-- **读/写热路径**索引，便于单独 EXPLAIN 比对：
--
--   | 索引 | 服务的查询 |
--   |------|-----------|
--   | uk_sys_notice_receiver | 扇出幂等（ON CONFLICT DO NOTHING 的冲突目标） |
--   | idx_sys_notice_receiver_unread | 未读数 / 全部已读（**部分索引**，只索引未读行） |
--   | idx_sys_notice_receiver_list | 我的通知分页（按时间倒序） |
--   | uk_sys_notice_idem | 业务事件幂等（同一 idempotency_key 只发一次） |
--   | idx_sys_notice_created / _biz | 管理端列表、按业务对象反查 |
--   | uk_sys_notice_delivery / _retry | 投递不重复 + 重试扫描 |
--
-- 注意：部分索引的谓词必须与查询条件**逐字可推断**，否则 PG 不会选它。
-- 未读数查询固定写 `WHERE user_id = ? AND read_flag = FALSE AND is_deleted = FALSE`；
-- 全部已读固定写 `WHERE user_id = ? AND read_flag = FALSE`（谓词成立，同样命中）。
-- ======================================================

-- ---------- 通知主体 ----------
CREATE UNIQUE INDEX uk_sys_notice_no ON sys_notice (notice_no);
CREATE UNIQUE INDEX uk_sys_notice_idem ON sys_notice (idempotency_key)
    WHERE idempotency_key IS NOT NULL;
CREATE INDEX idx_sys_notice_created ON sys_notice (created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_notice_biz ON sys_notice (biz_type, biz_id) WHERE biz_id IS NOT NULL;

-- ---------- 接收人 ----------
CREATE UNIQUE INDEX uk_sys_notice_receiver ON sys_notice_receiver (notice_id, user_id);
CREATE INDEX idx_sys_notice_receiver_unread ON sys_notice_receiver (user_id, notice_id)
    WHERE read_flag = FALSE AND is_deleted = FALSE;
CREATE INDEX idx_sys_notice_receiver_list ON sys_notice_receiver (user_id, created_at DESC)
    WHERE is_deleted = FALSE;

-- ---------- 投递记录 ----------
CREATE UNIQUE INDEX uk_sys_notice_delivery
    ON sys_notice_delivery (notice_id, COALESCE(receiver_id, notice_id), channel);
CREATE INDEX idx_sys_notice_delivery_retry ON sys_notice_delivery (next_retry_at)
    WHERE status IN ('PENDING', 'FAILED');
