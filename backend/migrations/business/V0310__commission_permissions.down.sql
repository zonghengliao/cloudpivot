-- ============================================================================
-- V0310 down：删授权 → 删 5 个按钮 → 删 2 个页面节点
--
-- 顺序：先删 sys_role_menu 上本卡注入的授权行（外键指向 sys_menu，先删子表），
-- 再删菜单；否则删菜单会被 sys_role_menu 的外键拦住（或者靠级联悄悄带走别人的授权）。
-- 只删本卡种下的固定 uuid 行 / 由本卡菜单派生出来的授权行，不碰任何别的模块。
--
-- ⚠ 不可逆点：回滚会撤掉所有角色对佣金两页的可见性（重新 up 可恢复，授权行重建）。
-- 幂等：全部 IF EXISTS / 固定集合，重复执行 no-op。
-- ============================================================================

DO $$
DECLARE
    v_menus uuid[] := ARRAY['20000000-0000-0000-0000-000000003301'::uuid,
                            '20000000-0000-0000-0000-000000003302'::uuid,
                            '20000000-0000-0000-0000-000000003303'::uuid,
                            '20000000-0000-0000-0000-000000003304'::uuid,
                            '20000000-0000-0000-0000-000000003311'::uuid,
                            '20000000-0000-0000-0000-000000003312'::uuid,
                            '20000000-0000-0000-0000-000000003313'::uuid];
    v_n integer;
BEGIN
    -- 0) 先撤「补历史缺口」那两条（business:flow:instance:read + approve → SALES_SUPERVISOR）：
    --    它们不在佣金七行菜单里，不先删会残留（角色恢复成 V0309 之前的状态）
    DELETE FROM sys_role_menu rm
     USING sys_role r, sys_menu m
     WHERE rm.role_id = r.id AND rm.menu_id = m.id
       AND r.role_code = 'SALES_SUPERVISOR'
       AND m.auth_code IN ('business:flow:instance:read', 'business:flow:approve');
    GET DIAGNOSTICS v_n = ROW_COUNT;
    RAISE NOTICE 'V0310 down：撤销 flow 缺口补丁 % 行', v_n;

    DELETE FROM sys_role_menu WHERE menu_id = ANY (v_menus);
    GET DIAGNOSTICS v_n = ROW_COUNT;
    RAISE NOTICE 'V0310 down：删除授权 % 行', v_n;

    DELETE FROM sys_menu WHERE id = ANY (v_menus);
    GET DIAGNOSTICS v_n = ROW_COUNT;
    RAISE NOTICE 'V0310 down：删除菜单 % 行', v_n;

    SELECT count(*) INTO v_n FROM sys_menu
     WHERE id = ANY (v_menus) OR auth_code LIKE 'business:erp:commission-%';
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0310 down 自检失败：佣金菜单仍残留 % 行', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM sys_menu m
     WHERE m.parent_id = '20000000-0000-0000-0000-000000003301'::uuid
        OR m.parent_id = '20000000-0000-0000-0000-000000003311'::uuid;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0310 down 自检失败：佣金页下仍有 % 个子节点', v_n;
    END IF;

    RAISE NOTICE 'V0310 down 完成：7 行菜单 + 其授权行已移除（角色本体不动 —— SALES_CLERK 由 V0267 建）';
END $$;
