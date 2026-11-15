-- ======================================================
-- V0108 回滚：撤掉 T2 的状态机 / 进度落点
--
-- 逆序：先撤授权与菜单节点（避免留下孤儿 sys_role_menu），再删列。
--
-- 提醒（评审常常问到）：
--   1. `biz_task_comment.comment_type` 被删后，T2 写过的 `STATUS_CHANGE` /
--      `PROGRESS_AUTO` 评论**仍留在表里**（行不删），只是失去了类型标记 ——
--      它们会退化成普通评论。回滚只用于「迁移写错了要重来」，
--      不要在生产上停在回滚态。
--   2. `biz_task.status_semantic` 本次从未填值（全是 NULL），删列不丢任何信息。
--   3. 菜单 id 固定（…0765–…0767），运维自己建过同名 auth_code 的行不会被误删。
-- ======================================================

-- ---------- ① 撤授权（先） ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000000765'::uuid,
                   '20000000-0000-0000-0000-000000000766'::uuid,
                   '20000000-0000-0000-0000-000000000767'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000000765'::uuid,
              '20000000-0000-0000-0000-000000000766'::uuid,
              '20000000-0000-0000-0000-000000000767'::uuid);

-- ---------- ② 删约束与列（逆序：先 comment_type，再 status_semantic） ----------
ALTER TABLE biz_task_comment DROP CONSTRAINT IF EXISTS ck_biz_task_comment_type;
ALTER TABLE biz_task_comment DROP COLUMN IF EXISTS comment_type;

ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS ck_biz_task_status_semantic;
ALTER TABLE biz_task DROP COLUMN IF EXISTS status_semantic;
