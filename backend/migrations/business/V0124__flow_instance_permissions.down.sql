-- ======================================================
-- V0124 down：审批流 · 实例侧权限节点（批次 D · D2）
--
-- 先撤授权（sys_role_menu）、再删菜单节点，末尾断言零残留。
-- 与 V0121 down 同款（D1 已验过的写法）。
-- ======================================================

DO $$
DECLARE
    ids uuid[] := ARRAY['20000000-0000-0000-0000-000000000800',
                        '20000000-0000-0000-0000-000000000801',
                        '20000000-0000-0000-0000-000000000802',
                        '20000000-0000-0000-0000-000000000803',
                        '20000000-0000-0000-0000-000000000804']::uuid[];
    left_menu  int;
    left_grant int;
BEGIN
    DELETE FROM sys_role_menu WHERE menu_id = ANY(ids);
    DELETE FROM sys_menu WHERE id = ANY(ids);

    SELECT count(*) INTO left_menu FROM sys_menu WHERE id = ANY(ids);
    SELECT count(*) INTO left_grant FROM sys_role_menu WHERE menu_id = ANY(ids);
    IF left_menu > 0 OR left_grant > 0 THEN
        RAISE EXCEPTION 'V0124 回滚不干净：菜单残留 % / 授权残留 %', left_menu, left_grant;
    END IF;

    RAISE NOTICE 'V0124 回滚自检通过：5 个权限节点与全部授权已清零';
END
$$;
