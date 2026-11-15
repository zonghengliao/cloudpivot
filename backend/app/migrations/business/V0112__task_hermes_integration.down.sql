-- ======================================================
-- V0112 回滚：撤掉 Hermes kanban 集成（批次 C+ · T5b）
--
-- 逆序：令牌表 → 评论类型值域收敛 → 附件 2 列 → biz_task 索引/约束/7 列。
--
-- 提醒（评审常问）：
--   ① `biz_task_ai_token` 整表 DROP：令牌是**短期凭据**，回滚后必须全部失效，
--      留在库里反而成了「云枢已经不认、但明文还在某个 kanban body 里」的残骸。
--   ② `exec_mode` 删列后，AI 任务退化成普通任务（历史信息只留在评论时间线里）：
--      因此回滚只用于「迁移写错了要重来」，不要在生产上停在回滚态。
--   ③ `comment_type` 收敛回 4 值前，先把 AI_REPORT / AI_PROGRESS 行改写成 PLAIN，
--      否则 ALTER 加约束会被存量行顶掉（T3 扩 ASSIGN 时同款处理，见 V0109 down）。
-- ======================================================

-- ---------- 1) 令牌表（先撤，它外键指向 biz_task） ----------
DROP INDEX IF EXISTS uk_biz_task_ai_token_hash;
DROP TABLE IF EXISTS biz_task_ai_token;

-- ---------- 2) 评论类型值域收敛回 4 值 ----------
UPDATE biz_task_comment
   SET comment_type = 'PLAIN'
 WHERE comment_type IN ('AI_REPORT', 'AI_PROGRESS');

ALTER TABLE biz_task_comment DROP CONSTRAINT IF EXISTS ck_biz_task_comment_type;
ALTER TABLE biz_task_comment
    ADD CONSTRAINT ck_biz_task_comment_type
    CHECK (comment_type IN ('PLAIN', 'STATUS_CHANGE', 'PROGRESS_AUTO', 'ASSIGN'));

COMMENT ON COLUMN biz_task_comment.comment_type IS
    '评论类型：PLAIN 普通 / STATUS_CHANGE 状态流转 / PROGRESS_AUTO 进度自动汇总 / ASSIGN 分派转派（T3）';

-- ---------- 3) 附件 2 列 ----------
ALTER TABLE biz_task_attachment DROP CONSTRAINT IF EXISTS ck_biz_task_attachment_source;
ALTER TABLE biz_task_attachment DROP COLUMN IF EXISTS ai_profile;
ALTER TABLE biz_task_attachment DROP COLUMN IF EXISTS source;

-- ---------- 4) biz_task：索引 → 约束 → 列 ----------
DROP INDEX IF EXISTS idx_biz_task_hermes;
ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS ck_biz_task_hermes_pair;
ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS ck_biz_task_exec_mode;
ALTER TABLE biz_task DROP COLUMN IF EXISTS ai_result;
ALTER TABLE biz_task DROP COLUMN IF EXISTS ai_summary;
ALTER TABLE biz_task DROP COLUMN IF EXISTS hermes_synced_at;
ALTER TABLE biz_task DROP COLUMN IF EXISTS hermes_status;
ALTER TABLE biz_task DROP COLUMN IF EXISTS hermes_task_id;
ALTER TABLE biz_task DROP COLUMN IF EXISTS hermes_board;
ALTER TABLE biz_task DROP COLUMN IF EXISTS exec_mode;
