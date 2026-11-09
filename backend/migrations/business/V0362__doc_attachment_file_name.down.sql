-- ============================================================
-- V0362 回滚：撤掉单据附件的文件名快照列（**E2E07-FIX-3**）
--
-- ⚠ 数据不可恢复：列里的文件名是「这次上传叫什么」的唯一记录 ——
--   回滚后读路径退回 `sys_file.original_name`，秒传复用过的附件会重新显示
--   **首个上传者**的文件名（就是本卡修掉的那个 bug）。真需要回滚时
--   **先** `\copy (SELECT id, file_name FROM biz_doc_attachment) TO ...` 存档。
--
-- 回滚后的行为：`backend/src/business/attachment/model.rs` 的 `COLUMNS` 会因列不存在
-- 直接 500 → 代码与 schema 必须**同时**回滚（迁移号回退 + 代码回退到本提交之前）。
-- 本文件只负责 schema 部分。
--
-- 幂等：DROP COLUMN IF EXISTS。
-- ============================================================

ALTER TABLE biz_doc_attachment
    DROP COLUMN IF EXISTS file_name;
