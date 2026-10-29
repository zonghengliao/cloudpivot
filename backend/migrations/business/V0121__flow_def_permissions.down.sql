-- ======================================================
-- V0121 回滚：审批流 · 定义侧权限节点（批次 D · D1）
--
-- 顺序：先撤授权（`sys_role_menu`），再删菜单节点 —— 反过来的话会留下
-- 指向空菜单的授权行（前端角色树显示「幽灵权限」）。
-- 种子迁移 `S__workflow_permissions.sql` 的授权也在本文件统一清理，保证回滚零残留。
-- ======================================================

DO $$
DECLARE
    ids uuid[] := ARRAY['20000000-0000-0000-0000-000000000780',
                        '20000000-0000-0000-0000-000000000781',
                        '20000000-0000-0000-0000-000000000782',
                        '20000000-0000-0000-0000-000000000783',
                        '20000000-0000-0000-0000-000000000784',
                        '20000000-0000-0000-0000-000000000785']::uuid[];
    left_menu  int;
    left_grant int;
BEGIN
    -- ---------- ① 撤授权 ----------
    DELETE FROM sys_role_menu WHERE menu_id = ANY(ids);

    -- ---------- ② 删菜单节点 ----------
    DELETE FROM sys_menu WHERE id = ANY(ids);

    -- ---------- ③ 自检：零残留 ----------
    SELECT count(*) INTO left_menu FROM sys_menu WHERE id = ANY(ids);
    SELECT count(*) INTO left_grant FROM sys_role_menu WHERE menu_id = ANY(ids);
    IF left_menu > 0 OR left_grant > 0 THEN
        RAISE EXCEPTION 'V0121 回滚不干净：菜单残留 % / 授权残留 %', left_menu, left_grant;
    END IF;

    RAISE NOTICE 'V0121 回滚自检通过：6 个菜单节点与全部授权已清零';
END
$$;
