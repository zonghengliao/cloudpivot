-- V0109 回滚：工作任务管理的分派 / 协作人权限节点
--
-- 顺序：先撤 `sys_role_menu` 授权，再删菜单节点 —— 反过来的话会留下指向空菜单的
-- 授权行（`sys_role_menu.menu_id` 有外键时直接删不动；没有外键则变成孤儿行，
-- 前端角色树会显示一个「幽灵权限」）。
--
-- 说明：种子迁移（S__task_permissions.sql）里给 SUPER_ADMIN 的那条授权**不在本文件**
-- 单独回滚 —— 它随菜单节点一起失效（下一个空库重建时种子会重新写）。
-- 这里统一清掉，保证「回滚零残留」。

DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000000768'::uuid,
                   '20000000-0000-0000-0000-000000000769'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000000768'::uuid,
              '20000000-0000-0000-0000-000000000769'::uuid);

-- ---------- ② 评论类型约束还原（去掉 ASSIGN） ----------
--
-- 库里可能已有 `comment_type='ASSIGN'` 的评论（转派写过）。直接还原成三值 CHECK
-- 会让 ALTER 失败（PG 会校验存量行）。这里按 V0108 回滚的同款口径处理：
-- **保留评论行与正文，只丢类型标记**（ASSIGN → PLAIN），不删任何数据。
UPDATE biz_task_comment SET comment_type = 'PLAIN' WHERE comment_type = 'ASSIGN';

ALTER TABLE biz_task_comment DROP CONSTRAINT IF EXISTS ck_biz_task_comment_type;
ALTER TABLE biz_task_comment
    ADD CONSTRAINT ck_biz_task_comment_type
    CHECK (comment_type IN ('PLAIN', 'STATUS_CHANGE', 'PROGRESS_AUTO'));

-- 自检：回滚后两个节点必须彻底消失、且不留授权行
DO $$
DECLARE
    left_menu  int;
    left_grant int;
BEGIN
    SELECT count(*) INTO left_menu
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000768'::uuid,
                  '20000000-0000-0000-0000-000000000769'::uuid);
    SELECT count(*) INTO left_grant
      FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000000768'::uuid,
                       '20000000-0000-0000-0000-000000000769'::uuid);

    IF left_menu > 0 OR left_grant > 0 THEN
        RAISE EXCEPTION 'V0109 回滚不干净：菜单残留 % 个、授权残留 % 行', left_menu, left_grant;
    END IF;
END
$$;
