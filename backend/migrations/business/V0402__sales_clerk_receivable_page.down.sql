-- ============================================================================
-- V0402 down：撤掉 SALES_CLERK × business:erp:receivable:list 这一行授权
--
-- 只删本迁移**可能会**加的那一对（角色按 role_code 现查、菜单按 auth_code 现查），
-- 不动 sys_menu / 其它授权。删完 SALES_CLERK 的 `statement:view` / `statement:export`
-- 会重新变成孤儿按钮 —— 这正是 V0402 之前的库状态（闸⑦ 红），回滚就该回到那里。
--
-- 安全阀：若该角色**还有别的页面码**依赖这次授权才能成立（例如同父页下的其它按钮码
-- 也被软删/改挂），回滚会让它们一起变孤儿 —— 这属于业务侧改了菜单结构，down 只打
-- NOTICE 提示，不擅自猜测、也不删别人的授权。
-- ============================================================================

DO $$
DECLARE
    v_role   uuid;
    v_menu   uuid;
    v_deleted int := 0;
    v_siblings int;
BEGIN
    SELECT id INTO v_role
      FROM sys_role
     WHERE role_code = 'SALES_CLERK' AND deleted_at IS NULL;
    IF v_role IS NULL THEN
        RAISE NOTICE 'V0402 down：SALES_CLERK 不存在或已软删 → 无需回滚';
        RETURN;
    END IF;

    SELECT id INTO v_menu
      FROM sys_menu
     WHERE auth_code = 'business:erp:receivable:list'
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF v_menu IS NULL THEN
        RAISE NOTICE 'V0402 down：父页面 receivable:list 不存在 → 无需回滚';
        RETURN;
    END IF;

    -- 同父页下的其它按钮码（诊断用）：回滚后它们会不会变孤儿，交给 NOTICE 说清楚
    SELECT count(DISTINCT m.auth_code) INTO v_siblings
      FROM sys_role_menu rm
      JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
     WHERE rm.role_id = v_role
       AND m.menu_type = 3
       AND m.parent_id = v_menu;

    DELETE FROM sys_role_menu
     WHERE role_id = v_role
       AND menu_id = v_menu;
    GET DIAGNOSTICS v_deleted = ROW_COUNT;

    IF v_siblings > 0 THEN
        RAISE NOTICE 'V0402 down：已删除授权 % 行；该角色另有 % 个按钮码挂在此父页下，回滚后会重新变成孤儿（V0402 之前的原始状态）',
            v_deleted, v_siblings;
    ELSE
        RAISE NOTICE 'V0402 down：已删除授权 % 行（无同父按钮依赖）', v_deleted;
    END IF;
END $$;
