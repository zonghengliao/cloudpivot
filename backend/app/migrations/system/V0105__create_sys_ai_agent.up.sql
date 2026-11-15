-- ======================================================
-- V0105：AI 用户（云枢 × Hermes · L1）—— `sys_ai_agent`
--
-- 依据：老大 2026-10-29「建用户的时候可以建 AI 用户，云枢自动建好 AI 用户的
--       profile，并且能编辑 AI 用户的职责和 soul.md」。
--
-- 回滚：V0105__create_sys_ai_agent.down.sql（DROP TABLE，零残留）
--
-- ── 为什么是 V0105 而不是卡面写的 V0092 ────────────────────────
-- 卡面写的 `V0092` 在 `migrations/system/` 里**确实空着**，但编号已推进到
-- V0102（`sys_user.user_type`）；此时补一个 V0092 会被迁移器判为「乱序补迁」
-- （启动日志 WARN，不阻断，但会让新环境与老环境的迁移顺序记录不一致）。
-- 编号是全仓共享资源（`migrations/{system,business}` 共用一个号段），
-- 实测此刻 V0104 被并行卡（T1 返工：`business/V0104__task_creator_index`）占用，
-- 因此按「下一个可用连续号」取 V0105；语义与卡面完全一致。
--
-- ── 与卡面 DDL 的差异（两处，都是为了让「删号后可重建同名」成立）──────────
-- 1. 增加 `deleted_at timestamptz`：删 AI 用户走**软删**（`sys_user` 也是软删，
--    两边口径一致，审计能查到「谁曾经是什么」）；
-- 2. `hermes_profile` 的唯一约束改为**部分唯一索引**（`WHERE deleted_at IS NULL`），
--    与 `sys_user.username` 的部分唯一索引同款 —— 否则软删后的行会永久占用 profile 名，
--    「删掉 agent-01 再建 agent-01」会被唯一约束挡死。
--
-- ── 与 sys_user 的关系 ────────────────────────────────────────
-- `user_id` 主键 + 外键 `ON DELETE CASCADE`：AI 用户与账号是 1:1，账号物理删除时配置行
-- 必须跟着消失（软删阶段由应用层同时软删两表，见 `business/aiagent/handler.rs`）。
--
-- ── 为什么 `hermes_profile` 上还有一条 CHECK ─────────────────
-- 应用层（`hermes::validate_profile_name`）已是第一道闸；数据库这一条是**第二道防线**：
-- 任何绕过应用层的写入（psql、别的服务、未来的批处理）都不可能塞进一个含
-- `;`、空格、`/`、引号的名字。正则与 Rust 侧逐字一致（`^[a-z][a-z0-9-]{1,30}$`）。
-- ======================================================

CREATE TABLE IF NOT EXISTS sys_ai_agent (
    -- 与 sys_user 1:1；账号物理删除时配置行级联删除
    user_id           uuid         PRIMARY KEY REFERENCES sys_user (id) ON DELETE CASCADE,
    -- Hermes profile 名（当前恒等于 sys_user.username）
    hermes_profile    varchar(64)  NOT NULL,
    -- 工作区绝对路径（<workspace_root>/<profile>/）
    workspace_path    varchar(500) NOT NULL,
    -- SOUL.md 全文（写入磁盘成功后落库）
    soul_md           text         NOT NULL DEFAULT '',
    -- 职责（同时写入 hermes profile describe）
    agent_description varchar(255),
    -- 模型覆盖（任意 JSON 对象；NULL 表示用 profile 自身 config.yaml 的模型）
    model_override    jsonb,
    -- 上次探活记录的 gateway 状态
    gateway_status    varchar(16)  NOT NULL DEFAULT 'unknown',
    created_at        timestamptz  NOT NULL DEFAULT now(),
    updated_at        timestamptz  NOT NULL DEFAULT now(),
    -- 软删标记（NULL = 有效）
    deleted_at        timestamptz,
    -- 取值约束：与 shared/aiagent 代码里的白名单逐字一致
    CONSTRAINT ck_sys_ai_agent_gateway_status
        CHECK (gateway_status IN ('unknown', 'online', 'offline')),
    -- 第二道防线：profile 名必须命中白名单正则（与 Rust 侧 PROFILE_NAME_PATTERN 一致）
    CONSTRAINT ck_sys_ai_agent_profile_name
        CHECK (hermes_profile ~ '^[a-z][a-z0-9-]{1,30}$')
);

-- 部分唯一索引：软删后允许重建同名 profile（与 sys_user.username 同款口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_sys_ai_agent_profile
    ON sys_ai_agent (hermes_profile)
    WHERE deleted_at IS NULL;

COMMENT ON TABLE  sys_ai_agent IS 'AI 用户（云枢 × Hermes · L1）：profile 生命周期与 SOUL.md 的库内副本';
COMMENT ON COLUMN sys_ai_agent.user_id IS '与 sys_user 1:1（AI 用户的账号行 user_type=ROBOT）';
COMMENT ON COLUMN sys_ai_agent.hermes_profile IS 'Hermes profile 名；CHECK 与 Rust 侧正则逐字一致（命令注入防线之一）';
COMMENT ON COLUMN sys_ai_agent.workspace_path IS '工作区绝对路径 /root/.hermes/workspace/<profile>/';
COMMENT ON COLUMN sys_ai_agent.soul_md IS 'SOUL.md 全文（磁盘写入成功后落库；磁盘是权威，库是副本）';
COMMENT ON COLUMN sys_ai_agent.agent_description IS '职责（同时是 hermes profile describe 的文本，kanban orchestrator 用它路由任务）';
COMMENT ON COLUMN sys_ai_agent.model_override IS '模型覆盖 JSON；NULL = 用 profile 自身 config.yaml 的模型';
COMMENT ON COLUMN sys_ai_agent.gateway_status IS '上次探活记录的 gateway 状态：unknown / online / offline';
COMMENT ON COLUMN sys_ai_agent.deleted_at IS '软删标记；部分唯一索引只约束未软删行，因此删号后可重建同名';
