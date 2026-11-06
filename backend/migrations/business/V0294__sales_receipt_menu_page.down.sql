-- ============================================================================
-- V0294 down：把收款单页还原成「未落地前端页」的状态
--
-- 只还原 V0294 自己改的三列（path / component / is_visible）+ title 文案：
--
--     path      → NULL
--     component → NULL
--     is_visible→ FALSE
--     title     → V0293 的原文案
--
-- **不删权限码、不删按钮、不删授权行** —— 那是 V0293 的 down 干的事。
-- 还原后页面在侧边栏不可见、点不到，但 5 个码仍然可授（`sys_menu` 承载行还在），
-- 后端 5 个接口照常工作（收款单不是只读页，但删前端页不影响任何已落库数据）。
--
-- 已知代价：若 down 之后有人手工在「菜单管理」里改过这行的 path/component，
-- 回滚会把它一起抹成 NULL。这是「菜单表没有来源列」的通病（与 V0291 down 同口径）。
-- ============================================================================

DO $$
DECLARE
    v_page     uuid := '20000000-0000-0000-0000-000000002890'::uuid;
    v_title    text := '收款单（销售链：RC 取号 / 4 态 / 一单核销多张应收）';
    v_children int;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = v_page) THEN
        RAISE EXCEPTION 'V0294 down 失败：收款单页面节点 …2890 不存在（V0293 已被回滚？）';
    END IF;

    -- ---------- ① 结构还原（幂等） ----------
    UPDATE sys_menu
       SET path = NULL, component = NULL, is_visible = FALSE,
           title = v_title, updated_at = now()
     WHERE id = v_page
       AND (path       IS DISTINCT FROM NULL
         OR component  IS DISTINCT FROM NULL
         OR is_visible IS DISTINCT FROM FALSE
         OR title      IS DISTINCT FROM v_title);

    -- ---------- ② 回滚后断言：三列已是 V0293 的状态 ----------
    IF EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = v_page
           AND (path IS NOT NULL OR component IS NOT NULL OR is_visible IS NOT FALSE)
    ) THEN
        RAISE EXCEPTION 'V0294 down 断言失败：收款单页未还原成 path=NULL / component=NULL / is_visible=FALSE';
    END IF;

    -- ---------- ③ 按钮不能被这一刀带走（权限码仍可授） ----------
    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = v_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_children <> 4 THEN
        RAISE EXCEPTION 'V0294 down 断言失败：收款单页下应有 4 个按钮，实际 % 个', v_children;
    END IF;

    RAISE NOTICE 'V0294 down 完成：收款单页已关闭（path/component=NULL，is_visible=FALSE），4 个按钮与 5 个权限码保留';
END $$;
