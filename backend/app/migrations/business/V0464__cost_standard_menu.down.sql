-- ============================================================================
-- V0464 回滚：标准成本维护页菜单 + 授权（M5-09）
--
-- 顺序：先删本迁移授的授权行（按本页 menu_id）→ 再删本页行。
-- 护栏：本页下若出现**本迁移之外的子菜单**（有人挂了按钮 / 子页），拒绝删除 ——
--       那是别人的东西，不该替人连坐删掉。
-- 幂等：删不到行不报错（DROP 语义），自检只在「形态不符」时抛异常。
-- ============================================================================

DO $$
DECLARE
    v_page_children integer;
BEGIN
    SELECT count(*) INTO v_page_children
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND parent_id = '20000000-0000-4000-8000-00000000d301'::uuid;
    IF v_page_children > 0 THEN
        RAISE EXCEPTION 'V0464 回滚拒绝：标准成本页下还挂着 % 条子菜单（不是本迁移建的），先人工确认', v_page_children;
    END IF;
END $$;

-- 1) 授权行（本页 menu_id）
DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-4000-8000-00000000d301'::uuid;

-- 2) 页面行
DELETE FROM sys_menu
 WHERE id = '20000000-0000-4000-8000-00000000d301'::uuid;
