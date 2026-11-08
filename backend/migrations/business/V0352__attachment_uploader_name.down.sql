-- ============================================================
-- V0352 回滚：撤掉两张附件表的上传人姓名快照列（**AUD-01**）
--
-- ⚠ 数据不可恢复：列里的姓名快照是唯一副本（用户改名后 sys_user 里已经没有旧名字），
--   drop 之后「改名前的上传人」这条信息永久丢失，只能靠
--   `operation_logs`（DOC_ATTACHMENT_UPLOAD 的 username）逐条反推。
--   真需要回滚时**先** `\copy (SELECT id, uploader_name FROM ...) TO ...` 存档。
--
-- 回滚后的行为：读路径的 `COALESCE(NULLIF(a.uploader_name,''), <实时 join>)` 第一步
-- 会取不到列 → 代码与 schema 必须同时回滚（`backend/src/business/attachment/model.rs`
-- 的 COLUMNS 与 `business/shared/user_name.rs` 的写入点）。本文件只负责 schema 部分。
--
-- 幂等：DROP COLUMN IF EXISTS。
-- ============================================================

ALTER TABLE biz_doc_attachment
    DROP COLUMN IF EXISTS uploader_name;

ALTER TABLE biz_task_attachment
    DROP COLUMN IF EXISTS uploader_name;
