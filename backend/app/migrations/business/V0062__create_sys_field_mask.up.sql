-- ======================================================
-- V0062：列级脱敏 · 新建 `sys_field_mask`（批次 B · B1）
--
-- 依据：docs/批次B-技术架构.md 附录 A.3（DDL 原文，已在 PG 18.6 实跑）；设计论证见 §3.1
-- 回滚：V0062__create_sys_field_mask.down.sql（DROP TABLE）
--
-- 三个设计点（都有实测依据，见架构文档 §3.1）：
-- 1. UNIQUE ... NULLS NOT DISTINCT（PG 15+）：让 role_id IS NULL 的全局规则也受唯一约束；
--    普通唯一索引下 NULL 互不相等，会存进多条互相覆盖的全局规则。
-- 2. CHECK 正则约束标识符：table_name/column_name 是本表唯一可能影响 SQL 标识符的字段。
-- 3. role_id 在规则表内（而非独立的角色关联表）：一条规则只属于「全局」或「一个角色」，
--    多角色用户按 OR 命中（脱敏是限制，取并集才安全）。
-- ======================================================

CREATE TABLE sys_field_mask (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    table_name VARCHAR(64) NOT NULL,                 -- 表名（小写下划线）
    column_name VARCHAR(64) NOT NULL,                -- 列名（小写下划线）
    mask_type SMALLINT NOT NULL,                     -- 1 手机号 2 身份证 3 邮箱 4 姓名 5 银行卡 6 金额 7 自定义正则
    mask_param VARCHAR(128),                         -- 类型相关参数，如手机号 '3,4'
    role_id UUID REFERENCES sys_role(id),            -- 适用角色；NULL = 全局默认
    status SMALLINT NOT NULL DEFAULT 1,              -- 1 启用 / 0 停用
    reason VARCHAR(255) NOT NULL,                    -- 脱敏理由（合规留痕）
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_field_mask_type CHECK (mask_type BETWEEN 1 AND 7),
    CONSTRAINT ck_sys_field_mask_ident CHECK (
        table_name ~ '^[a-z][a-z0-9_]{1,63}$' AND column_name ~ '^[a-z][a-z0-9_]{1,63}$')
);

COMMENT ON TABLE sys_field_mask IS '列级脱敏规则（表+列+脱敏方式，可按角色差异化）';
COMMENT ON COLUMN sys_field_mask.mask_type IS
    '脱敏类型：1 手机号 / 2 身份证 / 3 邮箱 / 4 姓名 / 5 银行卡 / 6 金额 / 7 自定义正则';
COMMENT ON COLUMN sys_field_mask.mask_param IS
    '类型参数：手机号/身份证/银行卡 "保留前N,保留后M"；姓名 "保留前N"；金额 "区间宽度"；正则 "pattern|group"';
COMMENT ON COLUMN sys_field_mask.role_id IS '适用角色；NULL 表示对所有人生效的全局默认规则';
COMMENT ON COLUMN sys_field_mask.reason IS '脱敏理由（合规留痕，必填）';

CREATE UNIQUE INDEX uk_sys_field_mask_rule
    ON sys_field_mask (table_name, column_name, role_id) NULLS NOT DISTINCT
    WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_field_mask_table
    ON sys_field_mask (table_name) WHERE deleted_at IS NULL;
