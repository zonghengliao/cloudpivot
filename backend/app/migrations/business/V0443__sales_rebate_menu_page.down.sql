-- ============================================================================
-- V0443 down：把返利页节点还原成 V0438 的终态（path / component 留 NULL、is_visible FALSE、
--              title「返利政策」、sort 85、parent …2701）
--
-- 不删任何 `sys_menu` 行、不删 `sys_role_menu` 授权 —— 节点的存在与 7 个按钮码都是
-- V0438 / V0439 / V0440 的地盘，本迁移只说「这一刀怎么补」，所以只回滚这一刀。
--
-- 幂等：每条 UPDATE 都带 `IS DISTINCT FROM`，在已是原始状态的库上重放 = 0 行更新。
-- 前置：节点必须在、且仍是页面节点（否则说明有人先回滚了 V0438，那是另一件事）。
-- ============================================================================

DO $$
DECLARE
    v_page    uuid := '20000000-0000-0000-0000-000000004380'::uuid;
    v_parent  uuid := '20000000-0000-0000-0000-000000002701'::uuid;
    v_title   text := '返利政策';
    v_sort    int  := 85;

    v_type    int;
    v_count   int;
    v_updated int := 0;
BEGIN
    SELECT menu_type INTO v_type
      FROM sys_menu
     WHERE id = v_page AND deleted_at IS NULL;
    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0443 down 前置失败：返利页节点 …4380 不存在或已软删（V0438 已回滚？）';
    END IF;
    IF v_type <> 2 THEN
        RAISE EXCEPTION 'V0443 down 前置失败：…4380 的 menu_type = %（应为 2）', v_type;
    END IF;

    -- 还原 path / component（V0438 原文：两列均留 NULL，等前端落地后由后续迁移补）
    UPDATE sys_menu
       SET path = NULL, component = NULL, updated_at = now()
     WHERE id = v_page
       AND (path IS NOT NULL OR component IS NOT NULL);
    GET DIAGNOSTICS v_updated = ROW_COUNT;
    RAISE NOTICE 'V0443 down ① path/component 还原 % 行', v_updated;

    -- 还原 title / parent / sort / 可见性
    UPDATE sys_menu
       SET title      = v_title,
           parent_id  = v_parent,
           sort_order = v_sort,
           is_visible = FALSE,
           updated_at = now()
     WHERE id = v_page
       AND (title      IS DISTINCT FROM v_title
         OR parent_id  IS DISTINCT FROM v_parent
         OR sort_order IS DISTINCT FROM v_sort
         OR is_visible IS DISTINCT FROM FALSE);
    GET DIAGNOSTICS v_updated = ROW_COUNT;
    RAISE NOTICE 'V0443 down ② title/parent/sort/visible 还原 % 行', v_updated;

    -- 自检：V0438 的终态（path/component 空 + 隐藏），且 7 个按钮仍挂在它下面
    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL
       AND menu_type = 2
       AND path IS NULL
       AND component IS NULL
       AND is_visible = FALSE
       AND title = v_title
       AND parent_id = v_parent
       AND sort_order = v_sort;
    IF v_count <> 1 THEN
        RAISE EXCEPTION 'V0443 down 自检失败：返利页节点未还原到 V0438 终态（命中 % 行，期望 1）', v_count;
    END IF;

    SELECT count(*) INTO v_count
      FROM sys_menu
     WHERE parent_id = v_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_count <> 7 THEN
        RAISE EXCEPTION 'V0443 down 自检失败：返利页节点下的按钮 = %（期望 7，回滚不该动它们）', v_count;
    END IF;

    RAISE NOTICE 'V0443 down 完成：返利页节点已还原为隐藏 + path/component 空（按钮与授权未动）';
END $$;
