-- ============================================================================
-- V0286 回滚：发货单页「收回」—— 把 path / component / is_visible / title 还原成 V0284 的种子态
--
-- 只关页面可见性、不删任何权限码：
--   · 3 个权限码与 2 个按钮是 V0284 的成果，回滚这一刀不许动它们；
--   · `sys_role_menu` 本迁移没写过，也没有需要回收的授权行。
--
-- 断言「页面下 2 个按钮仍在」：这条回滚的语义是「页面暂时不可见」，
-- 不是「把发货单能力从系统里摘掉」。按钮丢了说明有人在别处动了结构，要人看一眼。
-- ============================================================================

DO $$
DECLARE
    v_page      uuid := '20000000-0000-0000-0000-000000002841'::uuid;
    v_title     text := '发货单（销售链单据：单号 SH-yyyyMMdd-000001 / 4 态状态机 / 从销售订单带出未发完的行）。path/component/可见性由前端卡落地后补';
    v_children  int;
    n_bad       int;
BEGIN
    UPDATE sys_menu
       SET path = NULL,
           component = NULL,
           is_visible = FALSE,
           title = v_title,
           updated_at = now()
     WHERE id = v_page
       AND deleted_at IS NULL
       AND (path      IS NOT NULL
         OR component IS NOT NULL
         OR is_visible IS DISTINCT FROM FALSE
         OR title     IS DISTINCT FROM v_title);

    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL
       AND (path      IS NOT NULL
         OR component IS NOT NULL
         OR is_visible IS DISTINCT FROM FALSE);
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0286 回滚失败：发货单页的 path/component/is_visible 未还原成 NULL/NULL/FALSE';
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = v_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_children <> 2 THEN
        RAISE EXCEPTION 'V0286 回滚断言失败：发货单页下应有 2 个按钮，实际 % 个（回滚不该删权限码）', v_children;
    END IF;

    RAISE NOTICE 'V0286 回滚完成：发货单页已收回（path/component=NULL，is_visible=FALSE），2 个按钮仍在';
END $$;
