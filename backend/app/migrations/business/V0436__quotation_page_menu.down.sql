-- ============================================================================
-- V0436 down：把报价单页还原成 V0433 的「只有权限码、没有页面」状态
--
-- 只还原菜单形状（path / component / is_visible）：
--   path = NULL, component = NULL, is_visible = FALSE
--
-- **不删**任何 `sys_menu` 行、**不删**任何 `sys_role_menu` 授权 ——
-- 删权限码是 V0433 / V0434 down 的事，本文件回滚后那 6 个码与 5 个按钮必须原样还在
-- （末尾断言会验证，防止「回滚页面顺手把权限也删了」）。
-- ============================================================================

DO $$
DECLARE
    v_page     uuid := '20000000-0000-0000-0000-000000004331'::uuid;
    v_rows     integer;
    v_codes    integer;
    v_children integer;
BEGIN
    -- ---------- 0) 前置：节点必须还在 ----------
    SELECT count(*) INTO v_rows
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0436 down 前置失败：报价单页面节点 …4331 不存在或已软删';
    END IF;

    -- ---------- 1) 还原成 V0433 的形状（幂等：已经还原过就是 0 行） ----------
    UPDATE sys_menu
       SET path       = NULL,
           component  = NULL,
           is_visible = FALSE,
           updated_at = now()
     WHERE id = v_page
       AND (path IS NOT NULL
         OR component IS NOT NULL
         OR is_visible IS DISTINCT FROM FALSE);

    -- ---------- 2) 断言：三列已回空 ----------
    SELECT count(*) INTO v_rows
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL
       AND (path IS NOT NULL OR component IS NOT NULL OR is_visible IS DISTINCT FROM FALSE);
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'V0436 down 断言 A 失败：页面 path/component/is_visible 未回滚干净';
    END IF;

    -- ---------- 3) 断言：V0433 的 5 个码 / 4 个按钮一行不能少 ----------
    SELECT count(*) INTO v_codes
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND auth_code IN (
            'business:erp:quotation:list',
            'business:erp:quotation:create',
            'business:erp:quotation:update',
            'business:erp:quotation:delete',
            'business:erp:quotation:status'
       );
    IF v_codes <> 5 THEN
        RAISE EXCEPTION 'V0436 down 断言 B 失败：V0433 的 5 个权限码应还在，实际 % 行', v_codes;
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = v_page
       AND menu_type = 3
       AND deleted_at IS NULL
       AND auth_code IN (
            'business:erp:quotation:create',
            'business:erp:quotation:update',
            'business:erp:quotation:delete',
            'business:erp:quotation:status',
            'business:erp:quotation:convert'
       );
    IF v_children <> 5 THEN
        RAISE EXCEPTION 'V0436 down 断言 C 失败：报价单页下应仍有 5 个按钮（含 V0434 的 convert），实际 % 个', v_children;
    END IF;

    RAISE NOTICE 'V0436 down：报价单页已还原为隐藏（path/component 空），5 个码 + 5 个按钮仍在 ✓';
END $$;
