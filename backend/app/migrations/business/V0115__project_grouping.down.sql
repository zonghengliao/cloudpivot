-- ======================================================
-- V0115 回滚：项目分组（批次 C+ · T7c）
--
-- 顺序：① 撤授权 + 删菜单（否则会留下指向空菜单的 sys_role_menu 幽灵行）
--       ② 撤索引（唯一索引先撤，否则删列/改约束会被它挡住）
--       ③ 撤成员表
--       ④ 自检：零残留
--
-- ⚠️ **故意不回滚 `biz_task.project_id`**：那一列是 V0100 建的（T1），
--    本迁移没有改它的结构；但**数据**上如果用户已经把任务归进了项目，
--    down 之后这些任务的 `project_id` 会指向一个即将不存在的表。
--    因此本脚本先把 `biz_task.project_id` 置空（同 T7c 的「删项目不删任务」语义），
--    再删成员表 —— 任务本身一行都不动。
--
-- 说明：种子迁移 `S__project_permissions.sql` 的授权行不在那里单独回滚，
-- 本文件统一清掉，保证「回滚零残留」。
-- ======================================================

-- ---------- ① 撤授权 + 删菜单节点 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000000775'::uuid,
                   '20000000-0000-0000-0000-000000000776'::uuid,
                   '20000000-0000-0000-0000-000000000777'::uuid,
                   '20000000-0000-0000-0000-000000000778'::uuid,
                   '20000000-0000-0000-0000-000000000779'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000000775'::uuid,
              '20000000-0000-0000-0000-000000000776'::uuid,
              '20000000-0000-0000-0000-000000000777'::uuid,
              '20000000-0000-0000-0000-000000000778'::uuid,
              '20000000-0000-0000-0000-000000000779'::uuid);

-- ---------- ② 撤索引 ----------
DROP INDEX IF EXISTS uk_biz_project_no;
DROP INDEX IF EXISTS idx_biz_project_owner;
DROP INDEX IF EXISTS idx_biz_project_status;
DROP INDEX IF EXISTS idx_biz_project_member_user;

-- ---------- ③ 任务只解绑、不删（红线：删项目不删任务） ----------
UPDATE biz_task SET project_id = NULL WHERE project_id IS NOT NULL;

-- ---------- ④ 撤成员表 ----------
DROP TABLE IF EXISTS biz_project_member;

-- ---------- ⑤ 自检：回滚干净 ----------
DO $$
DECLARE
    left_menu  int;
    left_grant int;
    left_idx   int;
    left_tbl   int;
    left_bind  int;
BEGIN
    SELECT count(*) INTO left_menu
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000775'::uuid,
                  '20000000-0000-0000-0000-000000000776'::uuid,
                  '20000000-0000-0000-0000-000000000777'::uuid,
                  '20000000-0000-0000-0000-000000000778'::uuid,
                  '20000000-0000-0000-0000-000000000779'::uuid);
    SELECT count(*) INTO left_grant
      FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000000775'::uuid,
                       '20000000-0000-0000-0000-000000000776'::uuid,
                       '20000000-0000-0000-0000-000000000777'::uuid,
                       '20000000-0000-0000-0000-000000000778'::uuid,
                       '20000000-0000-0000-0000-000000000779'::uuid);
    SELECT count(*) INTO left_idx
      FROM pg_indexes
     WHERE indexname IN ('uk_biz_project_no',
                         'idx_biz_project_owner',
                         'idx_biz_project_status',
                         'idx_biz_project_member_user');
    SELECT count(*) INTO left_tbl
      FROM information_schema.tables
     WHERE table_name = 'biz_project_member';
    SELECT count(*) INTO left_bind
      FROM biz_task
     WHERE project_id IS NOT NULL;

    IF left_menu > 0 OR left_grant > 0 OR left_idx > 0 OR left_tbl > 0 OR left_bind > 0 THEN
        RAISE EXCEPTION
            'V0115 回滚不干净：菜单 % / 授权 % / 索引 % / 表 % / 未解绑任务 %',
            left_menu, left_grant, left_idx, left_tbl, left_bind;
    END IF;
END
$$;
