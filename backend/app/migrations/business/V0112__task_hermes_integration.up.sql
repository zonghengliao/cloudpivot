-- ======================================================
-- V0112：任务管理 · Hermes kanban 集成（批次 C+ · T5b）
--
-- 依据：docs/批次C+-Hermes集成设计-T5b.md（§二 任务级令牌 / §四 数据模型增量）
--       老大 2026-10-29 拍板 1A（AI 产出强制上传云枢）+ 2A（AI 完成后 → REVIEW）
-- 回滚：V0112__task_hermes_integration.down.sql（逆序，无残留）
--
-- ── 本次迁移做四件事 ────────────────────────────────────────────
-- ① `biz_task` 加 7 列：执行模式（HUMAN/AI）+ Hermes 三件套（board / task_id / status）
--    与同步时间戳 + AI 执行报告的两处落点（`ai_summary` 文本 / `ai_result` jsonb）。
-- ② `biz_task_attachment` 加 2 列：`source`（HUMAN/AI）与 `ai_profile`
--    —— 「AI 产出必须进云枢存储」这条红线要求附件能区分是谁产出的。
-- ③ 新建 `biz_task_ai_token`：**任务级短期令牌**。库内只存 `sha256(token)`，
--    明文只出现在派活时写进 Hermes kanban body 的那一份文本里。
-- ④ `biz_task_comment.comment_type` 值域扩 2 个：`AI_REPORT`（AI 执行报告）
--    与 `AI_PROGRESS`（AI 进度回写）。
--
-- ── 为什么 token 用 `task_id` 做主键（一任务一令牌） ─────────────────
-- 重新派活必须**让旧令牌立刻失效**：若允许一任务多令牌，重派后旧令牌仍能写本任务，
-- 「任务级」就不再是边界。主键 = task_id 让「重派 = 覆盖」成为唯一路径
-- （见 handler 里的 `ON CONFLICT (task_id) DO UPDATE`，同时把 `used_count` 归零）。
--
-- ── 为什么 `ai_result` 是 jsonb 而不是 text ───────────────────────────
-- 它的内容直接来自 `hermes kanban show --json` 的 `runs[0].metadata`
-- （交付清单：commit / migrations / endpoints / new_files）。前端按字段渲染，
-- 存 text 就得在前端二次 parse；`V0111` 的 changelog 用 jsonb 存 diff 是同款取舍。
-- 注意：写它必须用 `Value::Json(...)`，绑字符串到 jsonb 列会报
-- `error serializing parameter`（本仓库 A5 卡实测踩过，见 skill 9.12-1）。
-- ======================================================

-- ---------- 1) biz_task：执行模式 + Hermes 关联 + AI 结果 ----------
ALTER TABLE biz_task ADD COLUMN IF NOT EXISTS exec_mode VARCHAR(16) NOT NULL DEFAULT 'HUMAN';
ALTER TABLE biz_task ADD COLUMN IF NOT EXISTS hermes_board VARCHAR(32);
ALTER TABLE biz_task ADD COLUMN IF NOT EXISTS hermes_task_id VARCHAR(64);
ALTER TABLE biz_task ADD COLUMN IF NOT EXISTS hermes_status VARCHAR(32);
ALTER TABLE biz_task ADD COLUMN IF NOT EXISTS hermes_synced_at TIMESTAMPTZ;
ALTER TABLE biz_task ADD COLUMN IF NOT EXISTS ai_summary TEXT;
ALTER TABLE biz_task ADD COLUMN IF NOT EXISTS ai_result JSONB;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'biz_task'::regclass
           AND conname = 'ck_biz_task_exec_mode'
    ) THEN
        ALTER TABLE biz_task
            ADD CONSTRAINT ck_biz_task_exec_mode
            CHECK (exec_mode IN ('HUMAN', 'AI'));
    END IF;
    -- hermes_task_id 与 hermes_board 必须**同时**有值或同时为空：
    -- 只有 id 没有 board 时轮询会打到错误的看板（默认看板），表现为「任务查不到」。
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'biz_task'::regclass
           AND conname = 'ck_biz_task_hermes_pair'
    ) THEN
        ALTER TABLE biz_task
            ADD CONSTRAINT ck_biz_task_hermes_pair
            CHECK ((hermes_task_id IS NULL) = (hermes_board IS NULL));
    END IF;
END
$$;

COMMENT ON COLUMN biz_task.exec_mode IS
    '执行模式：HUMAN 人执行（默认）/ AI 由 Hermes kanban 上的 AI 员工执行（T5b）';
COMMENT ON COLUMN biz_task.hermes_board IS
    'Hermes 看板 slug（当前固定 dev）；与 hermes_task_id 同生同灭（CHECK ck_biz_task_hermes_pair）';
COMMENT ON COLUMN biz_task.hermes_task_id IS
    'Hermes kanban 任务 id（形如 t_4b7abf18）；派活成功后写入，轮询按它拉状态';
COMMENT ON COLUMN biz_task.hermes_status IS
    '最近一次轮询读到的 Hermes 原始状态（ready/running/done/failed/blocked/MISSING），仅用于排障与去重';
COMMENT ON COLUMN biz_task.hermes_synced_at IS '最近一次成功与 Hermes 同步的时间';
COMMENT ON COLUMN biz_task.ai_summary IS
    'AI 执行摘要冗余（= hermes latest_summary），列表页不必回表查评论';
COMMENT ON COLUMN biz_task.ai_result IS
    'AI 结构化结果（= hermes runs[0].metadata，jsonb），前端渲染「交付清单」卡片';

-- 轮询只扫「已派活」的任务，部分索引让这条查询与「按 hermes_task_id 反查」都走索引
CREATE INDEX IF NOT EXISTS idx_biz_task_hermes ON biz_task (hermes_task_id)
    WHERE hermes_task_id IS NOT NULL;
COMMENT ON INDEX idx_biz_task_hermes IS 'T5b 轮询：exec_mode=AI 且 hermes_task_id 非空的未终态任务';

-- ---------- 2) biz_task_attachment：来源区分（人 / AI） ----------
ALTER TABLE biz_task_attachment
    ADD COLUMN IF NOT EXISTS source VARCHAR(16) NOT NULL DEFAULT 'HUMAN';
ALTER TABLE biz_task_attachment
    ADD COLUMN IF NOT EXISTS ai_profile VARCHAR(64);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'biz_task_attachment'::regclass
           AND conname = 'ck_biz_task_attachment_source'
    ) THEN
        ALTER TABLE biz_task_attachment
            ADD CONSTRAINT ck_biz_task_attachment_source
            CHECK (source IN ('HUMAN', 'AI'));
    END IF;
END
$$;

COMMENT ON COLUMN biz_task_attachment.source IS
    '附件来源：HUMAN 人类上传（默认）/ AI 由 AI 员工产出（令牌上传或轮询回传的路径 B）';
COMMENT ON COLUMN biz_task_attachment.ai_profile IS
    '产出该附件的 Hermes profile 名（仅 source=AI 时有值，便于追责「这份产出是谁交的」）';

-- ---------- 3) 任务级短期令牌（明文只进 kanban body，库内只存 hash） ----------
CREATE TABLE IF NOT EXISTS biz_task_ai_token (
    task_id     UUID PRIMARY KEY REFERENCES biz_task(id) ON DELETE CASCADE,
    token_hash  VARCHAR(128) NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    used_count  INT NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE biz_task_ai_token IS
    '任务级短期令牌：AI 只能凭它读写**本任务**（附件/评论/进度/详情），一任务一令牌，重派即覆盖';
COMMENT ON COLUMN biz_task_ai_token.task_id IS '任务 id（biz_task.id，级联删除）；主键 = 「一任务一令牌」的强制约束';
COMMENT ON COLUMN biz_task_ai_token.token_hash IS 'sha256(明文令牌) 的十六进制；明文只在派活时写进 kanban body，永不入库';
COMMENT ON COLUMN biz_task_ai_token.expires_at IS '过期时间 = 派活时间 + max_runtime + 1h；过期即失效（中间件逐请求判定）';
COMMENT ON COLUMN biz_task_ai_token.used_count IS '累计使用次数（仅观测，不限流）；重派时归零';

-- 按 hash 反查令牌 → 任务：中间件每请求一次这类查询，必须走索引
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_task_ai_token_hash ON biz_task_ai_token (token_hash);
COMMENT ON INDEX uk_biz_task_ai_token_hash IS '令牌校验入口：hash → task_id（唯一，顺带防两个任务拿到同一 hash）';

-- ---------- 4) 评论类型扩容：AI 执行报告 / AI 进度 ----------
-- PG 不能给 CHECK 追加枚举值，只能 DROP + ADD（T3 扩 ASSIGN 时同款，见 V0109）
ALTER TABLE biz_task_comment DROP CONSTRAINT IF EXISTS ck_biz_task_comment_type;
ALTER TABLE biz_task_comment
    ADD CONSTRAINT ck_biz_task_comment_type
    CHECK (comment_type IN ('PLAIN', 'STATUS_CHANGE', 'PROGRESS_AUTO', 'ASSIGN',
                            'AI_REPORT', 'AI_PROGRESS'));

COMMENT ON COLUMN biz_task_comment.comment_type IS
    '评论类型：PLAIN 普通 / STATUS_CHANGE 状态流转 / PROGRESS_AUTO 进度自动汇总 / '
    'ASSIGN 分派转派（T3）/ AI_REPORT AI 执行报告（T5b）/ AI_PROGRESS AI 进度回写（T5b）';

-- ---------- 自检（失败即启动失败） ----------
-- 1) biz_task 的 7 列必须都在，`exec_mode` 的默认值必须是 HUMAN（存量任务全是人执行）；
-- 2) 两条 CHECK 必须在（exec_mode 值域 + hermes id/board 同生同灭）；
-- 3) 附件 2 列必须在，`source` 默认 HUMAN；
-- 4) 令牌表 + hash 唯一索引必须在；
-- 5) `comment_type` 的 CHECK 必须已包含 AI_REPORT / AI_PROGRESS（否则轮询写报告直接 500）。
DO $$
DECLARE
    col_name     text;
    found        int;
    default_text text;
    check_expr   text;
BEGIN
    FOR col_name IN
        SELECT unnest(ARRAY[
            'exec_mode', 'hermes_board', 'hermes_task_id', 'hermes_status',
            'hermes_synced_at', 'ai_summary', 'ai_result'
        ])
    LOOP
        IF NOT EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_name = 'biz_task' AND column_name = col_name
        ) THEN
            RAISE EXCEPTION 'V0112 自检失败：biz_task.% 不存在', col_name;
        END IF;
    END LOOP;

    SELECT column_default INTO default_text
      FROM information_schema.columns
     WHERE table_name = 'biz_task' AND column_name = 'exec_mode';
    IF default_text IS NULL OR default_text NOT LIKE '''HUMAN''%' THEN
        RAISE EXCEPTION 'V0112 自检失败：biz_task.exec_mode 默认值应为 ''HUMAN''，实际 %', default_text;
    END IF;

    SELECT count(*) INTO found
      FROM pg_constraint
     WHERE conrelid = 'biz_task'::regclass
       AND conname IN ('ck_biz_task_exec_mode', 'ck_biz_task_hermes_pair');
    IF found <> 2 THEN
        RAISE EXCEPTION 'V0112 自检失败：biz_task 的 2 条 CHECK 只找到 % 条', found;
    END IF;

    FOR col_name IN SELECT unnest(ARRAY['source', 'ai_profile'])
    LOOP
        IF NOT EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_name = 'biz_task_attachment' AND column_name = col_name
        ) THEN
            RAISE EXCEPTION 'V0112 自检失败：biz_task_attachment.% 不存在', col_name;
        END IF;
    END LOOP;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.tables WHERE table_name = 'biz_task_ai_token'
    ) THEN
        RAISE EXCEPTION 'V0112 自检失败：biz_task_ai_token 不存在';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
         WHERE tablename = 'biz_task_ai_token' AND indexname = 'uk_biz_task_ai_token_hash'
    ) THEN
        RAISE EXCEPTION 'V0112 自检失败：uk_biz_task_ai_token_hash 不存在';
    END IF;

    SELECT pg_get_constraintdef(oid) INTO check_expr
      FROM pg_constraint
     WHERE conrelid = 'biz_task_comment'::regclass
       AND conname = 'ck_biz_task_comment_type';
    IF check_expr IS NULL THEN
        RAISE EXCEPTION 'V0112 自检失败：ck_biz_task_comment_type 不存在';
    END IF;
    IF check_expr NOT LIKE '%AI_REPORT%' OR check_expr NOT LIKE '%AI_PROGRESS%' THEN
        RAISE EXCEPTION 'V0112 自检失败：comment_type 值域未包含 AI_REPORT/AI_PROGRESS（实际 %）', check_expr;
    END IF;

    RAISE NOTICE 'V0112 自检通过：biz_task 7 列 + 2 条 CHECK / 附件 2 列 / 令牌表 + 唯一索引 / 评论 6 种类型';
END
$$;
