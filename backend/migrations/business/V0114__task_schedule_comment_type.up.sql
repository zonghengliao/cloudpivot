-- ======================================================
-- V0114：工作任务管理 · 甘特拖拽改期（批次 C+ · T7a）
--
-- 依据：卡面 t_52ce6d07「一、后端 / 联动」+ docs/云枢设计修正补丁-20261029.md 修正 10
-- 回滚：V0114__task_schedule_comment_type.down.sql（逆序，零残留）
--
-- ── 本迁移只做一件事，说明「为什么必须动库」 ────────────────────
--
-- `biz_task_comment.comment_type` 扩值域，新增 `SCHEDULE`（改期留痕）。
--
-- 卡面要求改期成功后「写 changelog（old_start/old_due → new_start/new_due）+ 一条
-- `SCHEDULE` 类型评论」。库里 `ck_biz_task_comment_type` 由 V0112 固定为 6 值
-- （PLAIN / STATUS_CHANGE / PROGRESS_AUTO / ASSIGN / AI_REPORT / AI_PROGRESS），
-- 直接插 `SCHEDULE` 会撞 CHECK —— 整条改期事务回滚成 500 + 10004，
-- 而且日志里只会看到「check constraint violated」，看不出是评论类型没扩。
--
-- PG 不能给 CHECK 追加取值，只能 DROP + ADD（与 V0109 加 ASSIGN、V0112 加
-- AI_REPORT/AI_PROGRESS 同一做法）。**不新增表、不新增列、不新增权限码**：
-- 改期复用 `business:task:update`（卡面明确「复用，不新增」）。
--
-- 本文件刻意不改 `biz_task.start_date / due_date` 的任何约束：
-- 父子区间与 FS 依赖都是**软约束**（依赖在业务层判、跨行才能判），
-- 落成数据库约束需要触发器，与「依赖是软约束」的设计冲突
-- （核心设计 §四：依赖只做软约束，允许管理员强开并留痕）。
-- ======================================================

-- ---------- ① 扩 comment_type 值域（DROP + ADD，PG 不支持追加） ----------
ALTER TABLE biz_task_comment DROP CONSTRAINT IF EXISTS ck_biz_task_comment_type;
ALTER TABLE biz_task_comment
    ADD CONSTRAINT ck_biz_task_comment_type
    CHECK (comment_type IN ('PLAIN', 'STATUS_CHANGE', 'PROGRESS_AUTO', 'ASSIGN',
                            'AI_REPORT', 'AI_PROGRESS', 'SCHEDULE'));

COMMENT ON COLUMN biz_task_comment.comment_type IS
    '评论类型：PLAIN 普通 / STATUS_CHANGE 状态流转 / PROGRESS_AUTO 进度自动汇总 / '
    'ASSIGN 分派转派（T3）/ AI_REPORT AI 执行报告（T5b）/ AI_PROGRESS AI 进度回写（T5b）/ '
    'SCHEDULE 甘特拖拽改期（T7a）';

-- ---------- ② 自检：约束必须包含 SCHEDULE，且历史 6 值一个不少 ----------
DO $$
DECLARE
    check_expr TEXT;
BEGIN
    SELECT pg_get_constraintdef(c.oid) INTO check_expr
      FROM pg_constraint c
      JOIN pg_class t ON t.oid = c.conrelid
     WHERE t.relname = 'biz_task_comment'
       AND c.conname = 'ck_biz_task_comment_type';
    IF check_expr IS NULL THEN
        RAISE EXCEPTION 'V0114 自检失败：ck_biz_task_comment_type 不存在';
    END IF;
    IF check_expr NOT LIKE '%SCHEDULE%' THEN
        RAISE EXCEPTION 'V0114 自检失败：comment_type 值域未包含 SCHEDULE（实际 %）', check_expr;
    END IF;
    -- 老类型不许因为这次 DROP+ADD 静默丢失（存量行会立刻违反约束）
    IF check_expr NOT LIKE '%PLAIN%' OR check_expr NOT LIKE '%STATUS_CHANGE%'
       OR check_expr NOT LIKE '%PROGRESS_AUTO%' OR check_expr NOT LIKE '%ASSIGN%'
       OR check_expr NOT LIKE '%AI_REPORT%' OR check_expr NOT LIKE '%AI_PROGRESS%' THEN
        RAISE EXCEPTION 'V0114 自检失败：旧类型丢失（实际 %）', check_expr;
    END IF;

    RAISE NOTICE 'V0114 自检通过：comment_type 7 种类型（新增 SCHEDULE）';
END $$;
