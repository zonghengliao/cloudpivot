-- ======================================================
-- V0051：通知「定时发送」与投递状态（批次 A 遗留修复 · 项③）
--
-- 依据：`docs/批次A-技术架构.md` §4（消息通知）与 PRD N3「立即 / 定时」发送。
-- 本迁移**只新增列与索引**，不改 V0043 的任何既有列/约束（历史迁移指纹不可变）。
--
-- 设计要点
-- --------
-- * `scheduled_at`：计划发送时间；`NULL` = 立即发送（兼容 V0043 之前插入的历史行）。
-- * `dispatch_status`：投递状态机 —— 这一列是「未到点不可见」的实现载体：
--     PENDING  已创建、等待到点（**尚未扇出**：`sys_notice_receiver` 里没有行，
--              因此用户侧「我的通知」天然看不到，管理侧能看到任务与计划时间）
--     SENT     已扇出并投递（立即发送与定时到点后都是这个状态）
--     FAILED   到点后连续投递失败超过上限（由巡逻任务置位，不再重试）
--     CANCELED 定时通知在到点前被软删（`deleted_at` 非空）时由巡逻任务置位
--   历史行（V0043 建表后写入的）默认 `SENT`（它们本来就已经扇出过了）。
-- * `dispatch_attempts`：到点后的投递尝试次数（失败重试上限的计数器）。
-- * 部分索引 `idx_sys_notice_due`：巡逻任务只扫「PENDING 且到点」的行，
--   部分索引把扫描面压到「待发队列」这一小撮（与 V0049 未读索引同款思路）。
-- ======================================================

ALTER TABLE sys_notice ADD COLUMN scheduled_at TIMESTAMPTZ;
ALTER TABLE sys_notice ADD COLUMN dispatch_status VARCHAR(16) NOT NULL DEFAULT 'SENT';
ALTER TABLE sys_notice ADD COLUMN dispatch_attempts SMALLINT NOT NULL DEFAULT 0;

ALTER TABLE sys_notice
    ADD CONSTRAINT ck_sys_notice_dispatch_status
    CHECK (dispatch_status IN ('PENDING', 'SENT', 'FAILED', 'CANCELED'));

ALTER TABLE sys_notice
    ADD CONSTRAINT ck_sys_notice_dispatch_attempts
    CHECK (dispatch_attempts >= 0);

COMMENT ON COLUMN sys_notice.scheduled_at IS '计划发送时间（NULL = 立即发送）；到点前不扇出，用户侧不可见';
COMMENT ON COLUMN sys_notice.dispatch_status IS '投递状态：PENDING 待发（已建未扇出）/ SENT 已投递 / FAILED 重试耗尽 / CANCELED 到点前被取消';
COMMENT ON COLUMN sys_notice.dispatch_attempts IS '定时投递尝试次数（失败重试用，达上限置 FAILED）';

CREATE INDEX idx_sys_notice_due ON sys_notice (scheduled_at)
    WHERE dispatch_status = 'PENDING' AND deleted_at IS NULL;
