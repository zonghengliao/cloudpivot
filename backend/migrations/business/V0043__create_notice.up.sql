-- ======================================================
-- V0043：消息通知核心表（通知主体 / 接收人 / 多通道投递记录）
--
-- 依据：`docs/批次A-技术架构.md` §4.1~§4.4 与 §7 附录 A（V0043–V0045 DDL 原文）。
-- 本卡任务书的迁移文件名把三张核心表合并到 V0043（原设计 V0043/V0044/V0045 三档，
-- 通知模板单独放 V0044、权限种子放 V0045、索引放 V0049），表结构逐列与架构文档一致。
--
-- 关键约束（应用层不重复实现，数据库兜底）：
--   * ck_sys_notice_receiver_read  ：已读必有时间（否则阅读率口径会飘）
--   * ck_sys_notice_counts         ：read_count <= receiver_count（看板口径不会倒挂）
--   * ck_sys_notice_channel        ：channel_mask 落在 1~15（位掩码越界直接拒绝）
--   * ck_sys_notice_target         ：target_type 只允许 USER/ROLE/DEPT/ALL
--   * ck_sys_notice_delivery_*     ：通道 / 状态机 / 重试次数
--
-- 索引统一放 V0049（`V0049__notice_indexes`），与任务书交付物对齐。
-- ======================================================

-- ---------- 通知主体：一条通知 = 一次业务事件 ----------
CREATE TABLE sys_notice (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notice_no VARCHAR(32) NOT NULL,
    notice_type VARCHAR(32) NOT NULL DEFAULT 'SYSTEM',
    level VARCHAR(16) NOT NULL DEFAULT 'INFO',
    title VARCHAR(255) NOT NULL,
    content TEXT NOT NULL,
    content_format VARCHAR(8) NOT NULL DEFAULT 'TEXT',
    link_url VARCHAR(500),
    biz_type VARCHAR(64),
    biz_id VARCHAR(64),
    template_code VARCHAR(64),
    template_vars JSONB,
    sender_id UUID,
    sender_name VARCHAR(50),
    target_type VARCHAR(16) NOT NULL,
    target_ids JSONB,
    channel_mask SMALLINT NOT NULL DEFAULT 1,
    receiver_count INT NOT NULL DEFAULT 0,
    read_count INT NOT NULL DEFAULT 0,
    idempotency_key VARCHAR(128),
    status SMALLINT NOT NULL DEFAULT 1,
    expire_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_notice_level CHECK (level IN ('INFO', 'SUCCESS', 'WARN', 'ERROR')),
    CONSTRAINT ck_sys_notice_target CHECK (target_type IN ('USER', 'ROLE', 'DEPT', 'ALL')),
    CONSTRAINT ck_sys_notice_format CHECK (content_format IN ('TEXT', 'HTML', 'MARKDOWN')),
    CONSTRAINT ck_sys_notice_channel CHECK (channel_mask BETWEEN 1 AND 15),
    CONSTRAINT ck_sys_notice_counts CHECK (receiver_count >= 0 AND read_count >= 0
                                           AND read_count <= receiver_count)
);

COMMENT ON TABLE sys_notice IS '通知主体：一条通知 = 一次业务事件，接收人落在 sys_notice_receiver';
COMMENT ON COLUMN sys_notice.notice_no IS '通知编号（对外展示，NT + yyyyMMdd + 序列）';
COMMENT ON COLUMN sys_notice.notice_type IS '通知类型：SYSTEM 系统 / TODO 待办 / ALERT 告警 / WORKFLOW 流程 / CIRCULAR 公告';
COMMENT ON COLUMN sys_notice.level IS '级别：INFO / SUCCESS / WARN / ERROR';
COMMENT ON COLUMN sys_notice.content_format IS '正文格式：TEXT 纯文本 / HTML（入库前净化）/ MARKDOWN';
COMMENT ON COLUMN sys_notice.template_code IS '发送时使用的模板编码（可空：手工通知）';
COMMENT ON COLUMN sys_notice.template_vars IS '渲染变量 JSON，留痕用于复现（不含敏感信息）';
COMMENT ON COLUMN sys_notice.target_type IS '发送目标类型：USER 指定用户 / ROLE 角色 / DEPT 部门（含下级）/ ALL 全员';
COMMENT ON COLUMN sys_notice.target_ids IS '目标 ID 数组（UUID 字符串），ALL 时为空数组；仅留痕，实际接收人已扇出到 receiver 表';
COMMENT ON COLUMN sys_notice.channel_mask IS '通道位掩码：1 站内信 / 2 邮件 / 4 Webhook / 8 短信（本批只实现 1，其余留投递表与配置）';
COMMENT ON COLUMN sys_notice.receiver_count IS '扇出后的接收人数（冗余计数，供送达率看板）';
COMMENT ON COLUMN sys_notice.read_count IS '已读人数（冗余计数，约束保证 <= receiver_count）';
COMMENT ON COLUMN sys_notice.idempotency_key IS '幂等键（业务事件唯一标识），同一键只发一次通知';

-- ---------- 接收人：扇出后的每用户一行 ----------
CREATE TABLE sys_notice_receiver (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notice_id UUID NOT NULL REFERENCES sys_notice(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    read_flag BOOLEAN NOT NULL DEFAULT FALSE,
    read_at TIMESTAMPTZ,
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_sys_notice_receiver_read CHECK ((read_flag = FALSE AND read_at IS NULL)
                                                  OR (read_flag = TRUE AND read_at IS NOT NULL))
);

COMMENT ON TABLE sys_notice_receiver IS '通知接收人：扇出后的每用户一行，承载已读状态与用户侧删除';
COMMENT ON COLUMN sys_notice_receiver.read_flag IS '是否已读（CHECK 强制已读必有 read_at）';
COMMENT ON COLUMN sys_notice_receiver.is_deleted IS '用户侧删除（不影响其他接收人，物理行保留以便统计送达率）';

-- ---------- 多通道投递记录：本批只落状态机，不接网关 ----------
CREATE TABLE sys_notice_delivery (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notice_id UUID NOT NULL REFERENCES sys_notice(id) ON DELETE CASCADE,
    receiver_id UUID REFERENCES sys_notice_receiver(id) ON DELETE CASCADE,
    channel VARCHAR(16) NOT NULL,
    target VARCHAR(255) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    attempts SMALLINT NOT NULL DEFAULT 0,
    max_attempts SMALLINT NOT NULL DEFAULT 3,
    next_retry_at TIMESTAMPTZ,
    last_error VARCHAR(500),
    sent_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_sys_notice_delivery_channel CHECK (channel IN ('EMAIL', 'WEBHOOK', 'SMS')),
    CONSTRAINT ck_sys_notice_delivery_status CHECK (
        status IN ('PENDING', 'SENDING', 'SENT', 'FAILED', 'SKIPPED')),
    CONSTRAINT ck_sys_notice_delivery_attempts CHECK (attempts >= 0 AND attempts <= max_attempts)
);

COMMENT ON TABLE sys_notice_delivery IS '外部通道投递记录：本批只预留接口与状态机，不实现真实发送';
COMMENT ON COLUMN sys_notice_delivery.status IS '状态机：PENDING → SENDING → SENT / FAILED(重试) / SKIPPED(未配置)';
COMMENT ON COLUMN sys_notice_delivery.next_retry_at IS '下次重试时间（指数退避：1min / 5min / 25min）';
