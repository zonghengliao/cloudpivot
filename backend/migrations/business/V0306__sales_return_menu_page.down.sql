-- ============================================================================
-- V0306 down：把销售退货单页还原成 V0304 的「只有权限码、没有页面」状态
--
-- 只还原菜单形状（path / component / is_visible / title / parent / sort），
-- **不删**任何 `sys_menu` 行、**不删**任何 `sys_role_menu` 授权 ——
-- 删权限码是 V0304 down 的事，本文件回滚后 V0304 的 3 个码必须原样还在
-- （末尾断言会验证这一点，防止「回滚页面顺手把权限也删了」）。
--
-- 还原值逐字抄 V0304 的 INSERT：
--   path = NULL, component = NULL, is_visible = FALSE, parent_id = NULL, sort_order = 79,
--   title = '销售退货单（销售链单据：单号 SR-yyyyMMdd-000001 / 6 态状态机 / 从原发货单带出可退明细 / 超退 400）。path/component/可见性由前端卡落地后补'
-- ============================================================================

DO $$
DECLARE
    v_page      uuid := '20000000-0000-0000-0000-000000003041'::uuid;
    v_title     text := '销售退货单（销售链单据：单号 SR-yyyyMMdd-000001 / 6 态状态机 / 从原发货单带出可退明细 / 超退 400）。path/component/可见性由前端卡落地后补';
    v_sort      int  := 79;
    v_codes     int;
    v_children  int;
BEGIN
    -- ---------- 0) 前置：节点必须还在（否则无事可回滚，直接报错让人看清） ----------
    SELECT count(*) INTO v_codes
      FROM sys_menu
     WHERE id = v_page AND deleted_at IS NULL;
    IF v_codes <> 1 THEN
        RAISE EXCEPTION 'V0306 down 前置失败：销售退货单页面节点 …3041 不存在或已软删';
    END IF;

    -- ---------- 1) 还原成 V0304 的形状（幂等） ----------
    UPDATE sys_menu
       SET path       = NULL,
           component  = NULL,
           is_visible = FALSE,
           title      = v_title,
           parent_id  = NULL,
           sort_order = v_sort,
           updated_at = now()
     WHERE id = v_page
       AND (path       IS NOT NULL
         OR component  IS NOT NULL
         OR is_visible IS DISTINCT FROM FALSE
         OR title      IS DISTINCT FROM v_title
         OR parent_id  IS NOT NULL
         OR sort_order IS DISTINCT FROM v_sort);

    -- ---------- 2) 断言：页面三列已回空、可见性已关 ----------
    SELECT count(*) INTO v_codes
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL
       AND (path IS NOT NULL OR component IS NOT NULL OR is_visible IS DISTINCT FROM FALSE);
    IF v_codes <> 0 THEN
        RAISE EXCEPTION 'V0306 down 断言 A 失败：页面 path/component/is_visible 未回滚干净';
    END IF;

    -- ---------- 3) 断言：V0304 的 3 个权限码一行不能少 ----------
    SELECT count(*) INTO v_codes
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND auth_code IN (
              'business:erp:sales-return:list',
              'business:erp:sales-return:create',
              'business:erp:sales-return:status'
           );
    IF v_codes <> 3 THEN
        RAISE EXCEPTION 'V0306 down 断言 B 失败：退货单权限码应为 3 个，实际 % 个（回滚不能删权限码，那是 V0304 的事）', v_codes;
    END IF;

    -- ---------- 4) 断言：2 个按钮仍挂在页面下 ----------
    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = v_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_children <> 2 THEN
        RAISE EXCEPTION 'V0306 down 断言 C 失败：页面下应有 2 个按钮，实际 % 个', v_children;
    END IF;

    RAISE NOTICE 'V0306 down 完成：销售退货单页已还原（path/component=NULL, is_visible=FALSE, parent=NULL, sort=%），3 个权限码与 2 个按钮完好', v_sort;
END $$;
