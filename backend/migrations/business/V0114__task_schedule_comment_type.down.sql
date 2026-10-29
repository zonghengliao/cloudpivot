-- ======================================================
-- V0114 回滚：甘特拖拽改期（批次 C+ · T7a）
--
-- 顺序：先把 SCHEDULE 评论降级成 PLAIN，再还原 6 值 CHECK。
-- 反过来的话存量 SCHEDULE 行会立刻违反新约束，ALTER 直接失败。
-- 本迁移只扩了 comment_type 值域，没有表/列/权限码，所以回滚只有这两步。
-- ======================================================

-- ---------- ① 存量 SCHEDULE 评论降级（信息不丢：正文里已经有「改期：… 」） ----------
UPDATE biz_task_comment SET comment_type = 'PLAIN' WHERE comment_type = 'SCHEDULE';

-- ---------- ② 还原 V0112 的 6 值 CHECK ----------
ALTER TABLE biz_task_comment DROP CONSTRAINT IF EXISTS ck_biz_task_comment_type;
ALTER TABLE biz_task_comment
    ADD CONSTRAINT ck_biz_task_comment_type
    CHECK (comment_type IN ('PLAIN', 'STATUS_CHANGE', 'PROGRESS_AUTO', 'ASSIGN',
                            'AI_REPORT', 'AI_PROGRESS'));

COMMENT ON COLUMN biz_task_comment.comment_type IS
    '评论类型：PLAIN 普通 / STATUS_CHANGE 状态流转 / PROGRESS_AUTO 进度自动汇总 / '
    'ASSIGN 分派转派（T3）/ AI_REPORT AI 执行报告（T5b）/ AI_PROGRESS AI 进度回写（T5b）';
