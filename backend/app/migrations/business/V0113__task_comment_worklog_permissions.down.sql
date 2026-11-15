-- ======================================================
-- V0113 回滚：评论 / 附件 / 工时补齐（批次 C+ · T6b）
--
-- 顺序：先撤授权、再删菜单，最后撤 schema —— 反过来的话会留下指向空菜单的
-- 授权行（前端角色树显示「幽灵权限」）。
-- 说明：种子迁移 `S__task_permissions.sql` 第 4 段给 SUPER_ADMIN 的授权不在
-- 那里单独回滚，本文件统一清掉，保证「回滚零残留」。
-- ======================================================

-- ---------- ① 撤授权 + 删菜单节点 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000000772'::uuid,
                   '20000000-0000-0000-0000-000000000773'::uuid,
                   '20000000-0000-0000-0000-000000000774'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000000772'::uuid,
              '20000000-0000-0000-0000-000000000773'::uuid,
              '20000000-0000-0000-0000-000000000774'::uuid);

-- ---------- ② 撤约束 / 索引 / 列 ----------
ALTER TABLE biz_task_worklog DROP CONSTRAINT IF EXISTS ck_biz_task_worklog_hours_range;

DROP INDEX IF EXISTS idx_biz_task_worklog_task_date;
DROP INDEX IF EXISTS idx_biz_task_comment_task_time;

-- 附件的删除记录随列一起消失（绑定行本身保留 —— 它们是「任务 ↔ 文件」的事实，
-- 不属于本迁移新增的语义）。回滚后「已软删的附件」会重新出现在列表里，
-- 这是 down 的固有代价：本迁移之前系统没有软删概念。
ALTER TABLE biz_task_attachment DROP COLUMN IF EXISTS deleted_at;

-- ---------- ③ 自检：回滚干净 ----------
DO $$
DECLARE
    left_menu  int;
    left_grant int;
    left_col   int;
    left_idx   int;
    left_ck    int;
BEGIN
    SELECT count(*) INTO left_menu
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000772'::uuid,
                  '20000000-0000-0000-0000-000000000773'::uuid,
                  '20000000-0000-0000-0000-000000000774'::uuid);
    SELECT count(*) INTO left_grant
      FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000000772'::uuid,
                       '20000000-0000-0000-0000-000000000773'::uuid,
                       '20000000-0000-0000-0000-000000000774'::uuid);
    SELECT count(*) INTO left_col
      FROM information_schema.columns
     WHERE table_name = 'biz_task_attachment' AND column_name = 'deleted_at';
    SELECT count(*) INTO left_idx
      FROM pg_indexes
     WHERE indexname IN ('idx_biz_task_comment_task_time',
                         'idx_biz_task_worklog_task_date');
    SELECT count(*) INTO left_ck
      FROM pg_constraint
     WHERE conrelid = 'biz_task_worklog'::regclass
       AND conname = 'ck_biz_task_worklog_hours_range';

    IF left_menu > 0 OR left_grant > 0 OR left_col > 0 OR left_idx > 0 OR left_ck > 0 THEN
        RAISE EXCEPTION
            'V0113 回滚不干净：菜单残留 % / 授权残留 % / 列残留 % / 索引残留 % / 约束残留 %',
            left_menu, left_grant, left_col, left_idx, left_ck;
    END IF;
END
$$;
