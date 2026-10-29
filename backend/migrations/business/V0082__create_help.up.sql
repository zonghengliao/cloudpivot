-- ======================================================
-- V0082：帮助文档（批次 C · C4）三表
--
-- 依据：docs/批次C-技术架构.md §3.1（表设计）、附录 A.3（DDL 原文）、§7 C4 行
-- 回滚：V0082__create_help.down.sql
--
-- ── 表职责 ────────────────────────────────────────────
--   sys_help_category      分类树（parent_id 自引用 + 应用层限深 3 层）
--   sys_help_doc           文档主体（Markdown 源 + 净化后的 HTML 快照）
--   sys_help_doc_version   版本快照（发布 / 保存 / 回滚时留全文，保留最近 20 版）
--
-- ── 三个刻意决策（与架构逐字一致，别"顺手优化"）────────
--  ① 分类树用**自引用 + 递归 CTE**，不用 `path` 物化路径：项目已有
--     `system/dept` 的 `dept_path LIKE` 搬迁不重算的坑（架构 §3.1）。
--     "深度 ≤ 3" 无法用 CHECK 表达，由 model 层递归 CTE 校验 + 单测断言。
--  ② `slug` 唯一索引是**部分索引**（`WHERE deleted_at IS NULL`）：软删后
--     同名 slug 可复用（对外 URL 稳定，不因一次误删永久占用）。
--  ③ `ck_sys_help_doc_publish` 把「发布必有发布时间」下沉到数据库：
--     应用层漏写 `published_at` 会直接 500，而不是产出一条"发布了但时间未知"
--     的脏数据（H-04 断言的正是这条约束）。
--
-- ── 索引分布 ──────────────────────────────────────────
-- 本迁移只建表 + 约束；**全部索引在 V0083**（含 pg_trgm 升级预案的注释态），
-- 便于"只回滚索引、不动数据"这一运维动作。
-- ======================================================

-- ---------- 1) 分类树 ----------
CREATE TABLE sys_help_category (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id   UUID REFERENCES sys_help_category(id) ON DELETE RESTRICT,
    name        VARCHAR(100) NOT NULL,
    code        VARCHAR(64)  NOT NULL,
    icon        VARCHAR(64),
    description VARCHAR(500),
    sort_order  INT NOT NULL DEFAULT 0,
    status      SMALLINT NOT NULL DEFAULT 1,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at  TIMESTAMPTZ,
    CONSTRAINT ck_sys_help_category_status CHECK (status IN (0, 1)),
    CONSTRAINT ck_sys_help_category_code   CHECK (code ~ '^[a-z][a-z0-9_-]{1,63}$'),
    -- 自引用树不允许"自己是自己的父"（更深的自环由应用层递归校验兜住）
    CONSTRAINT ck_sys_help_category_self   CHECK (parent_id IS NULL OR parent_id <> id)
);

COMMENT ON TABLE  sys_help_category IS '帮助文档分类树（自引用 + 应用层限深 3 层；查询用递归 CTE）';
COMMENT ON COLUMN sys_help_category.code IS '分类编码（租户内唯一，软删后可复用）：^[a-z][a-z0-9_-]{1,63}$';

-- ---------- 2) 文档主体 ----------
CREATE TABLE sys_help_doc (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    category_id  UUID NOT NULL REFERENCES sys_help_category(id) ON DELETE RESTRICT,
    title        VARCHAR(200) NOT NULL,
    slug         VARCHAR(120) NOT NULL,
    summary      VARCHAR(500) NOT NULL DEFAULT '',
    content_md   TEXT NOT NULL,
    content_html TEXT NOT NULL DEFAULT '',
    keywords     VARCHAR(255),
    version      INT NOT NULL DEFAULT 1,
    status       VARCHAR(16) NOT NULL DEFAULT 'DRAFT',
    sort_order   INT NOT NULL DEFAULT 0,
    view_count   INT NOT NULL DEFAULT 0,
    published_at TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    update_by    UUID,
    deleted_at   TIMESTAMPTZ,
    CONSTRAINT ck_sys_help_doc_status  CHECK (status IN ('DRAFT', 'PUBLISHED', 'OFFLINE')),
    CONSTRAINT ck_sys_help_doc_slug    CHECK (slug ~ '^[a-z0-9][a-z0-9-]{1,119}$'),
    CONSTRAINT ck_sys_help_doc_body    CHECK (octet_length(content_md) BETWEEN 1 AND 262144),
    -- 状态与发布时间的一致性（H-04）：DRAFT 必须没有发布时间，其余状态必须有
    CONSTRAINT ck_sys_help_doc_publish CHECK (
        (status = 'DRAFT' AND published_at IS NULL)
        OR (status <> 'DRAFT' AND published_at IS NOT NULL)),
    CONSTRAINT ck_sys_help_doc_version CHECK (version >= 0),
    CONSTRAINT ck_sys_help_doc_views   CHECK (view_count >= 0)
);

COMMENT ON TABLE  sys_help_doc IS '帮助文档（Markdown 源 + 净化后的 HTML 快照；content_html 只能由 shared/content 生成）';
COMMENT ON COLUMN sys_help_doc.slug IS '对外稳定 URL 片段：^[a-z0-9][a-z0-9-]{1,119}$（软删后可复用）';
COMMENT ON COLUMN sys_help_doc.version IS '当前版本号：0 = 从未保存过版本快照的初始草稿；每次保存/发布/回滚 +1 并与版本表对齐';
COMMENT ON COLUMN sys_help_doc.view_count IS '阅读量（按用户/日去重累加，见 handler::read）';

-- ---------- 3) 版本快照 ----------
CREATE TABLE sys_help_doc_version (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_id       UUID NOT NULL REFERENCES sys_help_doc(id) ON DELETE CASCADE,
    version      INT NOT NULL,
    title        VARCHAR(200) NOT NULL,
    content_md   TEXT NOT NULL,
    content_html TEXT NOT NULL DEFAULT '',
    change_note  VARCHAR(500),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    CONSTRAINT uk_sys_help_doc_version UNIQUE (doc_id, version),
    CONSTRAINT ck_sys_help_doc_version_no CHECK (version > 0)
);

COMMENT ON TABLE sys_help_doc_version IS '文档版本快照：保存/发布/回滚时留一份全文用于一键回滚；保留最近 20 版，超出由定时任务 help_version_cleanup 删除';
