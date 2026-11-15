-- ======================================================
-- V0080：公告管理（主体 / 发布范围 / 已读回执 / 附件）
-- 依据：docs/批次C-技术架构.md §2.3（表设计与数据权限结论）、§6 附录 A.1（**已在 PG 18.6 实跑通过**）
-- 回滚：V0080__create_announcement.down.sql
--
-- ── 与 §2.1 边界的对应 ─────────────────────────────────────────
-- 本表只回答「这份公告是什么、给谁看、什么时候有效」；
-- 「已经送到谁的信箱里、他读了没有」由批次 A 的 sys_notice / sys_notice_receiver 承担。
--
-- ── 为什么 content_md 是唯一真源、content_html 只是快照 ────────
-- content_html 由服务端渲染 + 净化后写入（闸 1 / 闸 2），读取侧（C2）出参前**再净化一次**
-- （闸 3）。三处都过 business::shared::content（Recipe::Announcement），
-- 任何一处都不允许用 Markdown 原文拼 HTML。
--
-- ── 刻意不加的两类外键 ────────────────────────────────────────
--   ① sys_announcement.notice_id：sys_notice 的清理任务会**硬删**过期通知，
--      加外键就必须 ON DELETE SET NULL，而「公告 → 投递」本是弱引用（投递过期后公告仍在）；
--   ② sys_announcement_scope.target_id / sys_announcement_read.user_id /
--      sys_announcement_attachment.file_id：跨模块弱引用（部门 / 用户 / 文件），
--      与底座其它业务表同口径，避免底座表的清理被业务表阻断。
--
-- ── 数据权限 ─────────────────────────────────────────────────
-- 三张新表**有意不登记** shared/scope.rs 的 SCOPED_TABLES（架构 §2.3.3 结论）：
-- 公告没有部门归属列，可见性只由 scope_type + sys_announcement_scope 判定。
-- 列表接口走后端 C2 的 scope::paginate_unscoped(ScopeTarget::unscoped("sys_announcement"))。
-- ======================================================

CREATE TABLE sys_announcement (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    ann_no          VARCHAR(32)  NOT NULL,
    title           VARCHAR(200) NOT NULL,
    summary         VARCHAR(500) NOT NULL DEFAULT '',
    content_md      TEXT         NOT NULL,
    content_html    TEXT         NOT NULL DEFAULT '',
    content_format  VARCHAR(8)   NOT NULL DEFAULT 'MARKDOWN',
    level           VARCHAR(16)  NOT NULL DEFAULT 'NORMAL',
    scope_type      VARCHAR(8)   NOT NULL DEFAULT 'ALL',
    is_top          BOOLEAN      NOT NULL DEFAULT FALSE,
    top_expire_at   TIMESTAMPTZ,
    status          VARCHAR(16)  NOT NULL DEFAULT 'DRAFT',
    publish_at      TIMESTAMPTZ,
    expire_at       TIMESTAMPTZ,
    publisher_id    UUID,
    publisher_name  VARCHAR(50),
    notice_id       UUID,
    receiver_count  INT          NOT NULL DEFAULT 0,
    read_count      INT          NOT NULL DEFAULT 0,
    version         INT          NOT NULL DEFAULT 1,
    remark          VARCHAR(500),
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT ck_sys_announcement_status  CHECK (status IN ('DRAFT','PUBLISHED','OFFLINE','EXPIRED')),
    CONSTRAINT ck_sys_announcement_level   CHECK (level IN ('NORMAL','IMPORTANT','URGENT')),
    CONSTRAINT ck_sys_announcement_scope   CHECK (scope_type IN ('ALL','ROLE','DEPT','USER')),
    CONSTRAINT ck_sys_announcement_format  CHECK (content_format IN ('MARKDOWN','HTML')),
    CONSTRAINT ck_sys_announcement_publish CHECK (
        (status = 'DRAFT' AND publish_at IS NULL) OR (status <> 'DRAFT' AND publish_at IS NOT NULL)),
    CONSTRAINT ck_sys_announcement_expire  CHECK (expire_at IS NULL OR publish_at IS NULL OR expire_at > publish_at),
    CONSTRAINT ck_sys_announcement_top     CHECK (is_top OR top_expire_at IS NULL),
    CONSTRAINT ck_sys_announcement_counts  CHECK (receiver_count >= 0 AND read_count >= 0
                                                  AND read_count <= receiver_count),
    CONSTRAINT ck_sys_announcement_body    CHECK (octet_length(content_md) BETWEEN 1 AND 131072)
);

COMMENT ON TABLE  sys_announcement IS '公告主体：内容（Markdown 源 + 渲染快照）；投递由 sys_notice 承担';
COMMENT ON COLUMN sys_announcement.content_md   IS 'Markdown 源（唯一真源，128 KiB 上限）';
COMMENT ON COLUMN sys_announcement.content_html IS '服务端渲染 + ammonia 净化后的 HTML 快照（写时生成，读时再净化）';
COMMENT ON COLUMN sys_announcement.notice_id    IS '发布时扇出的 sys_notice.id（关闭扇出时为空；不加外键：通知会被清理任务硬删）';
COMMENT ON COLUMN sys_announcement.version      IS '内容版本：每次发布 +1，参与 notice 幂等键 announcement:{id}:v{version}';
COMMENT ON COLUMN sys_announcement.status       IS 'DRAFT 草稿 / PUBLISHED 已发布 / OFFLINE 人工下线 / EXPIRED 到期自动';

CREATE TABLE sys_announcement_scope (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    announcement_id UUID NOT NULL REFERENCES sys_announcement(id) ON DELETE CASCADE,
    scope_type      VARCHAR(8) NOT NULL,
    target_id       UUID NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_sys_announcement_scope_type CHECK (scope_type IN ('ROLE','DEPT','USER')),
    CONSTRAINT uk_sys_announcement_scope UNIQUE (announcement_id, scope_type, target_id)
);
COMMENT ON TABLE sys_announcement_scope IS '发布范围明细：DEPT 已按发布时刻展开为「含下级的部门 ID 集合」；读取侧只做 ANY(array) 判定';

CREATE TABLE sys_announcement_read (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    announcement_id UUID NOT NULL REFERENCES sys_announcement(id) ON DELETE CASCADE,
    user_id         UUID NOT NULL,
    read_at         TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT uk_sys_announcement_read UNIQUE (announcement_id, user_id)
);
COMMENT ON TABLE sys_announcement_read IS '已读回执：只记已读（未读 = 可见集合 - 已读集合），避免为全员预写 N 行';

CREATE TABLE sys_announcement_attachment (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    announcement_id UUID NOT NULL REFERENCES sys_announcement(id) ON DELETE CASCADE,
    file_id         UUID NOT NULL,
    file_name       VARCHAR(255) NOT NULL,
    file_size       BIGINT NOT NULL DEFAULT 0,
    mime_type       VARCHAR(128),
    sort_order      INT NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT uk_sys_announcement_attachment UNIQUE (announcement_id, file_id)
);
COMMENT ON TABLE sys_announcement_attachment IS '公告附件：引用 sys_file（批次 A 存储）；下载必须经公告可见性校验的代理接口';
