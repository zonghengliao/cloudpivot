-- ============================================================================
-- V0282：销售订单页「落地收尾」—— 把 V0265 故意留空的 path / component / is_visible 补齐
--
-- 依据：kanban 卡 t_1ae4c684（SO-07 销售订单前端页面），承接 t_860ea3d4（SO-06 审批流）。
--
-- # 一、为什么分两步（V0265 留 NULL → 本迁移补全）
--   `system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router`（真库档硬门禁）
--   要求：`menu_type = 2 AND path IS NOT NULL AND path <> ''` 的每一行，path 都必须能在
--   `frontend/src/router/index.ts` 里解析到。SO-05 只交付后端 API，那时前端路由还不存在
--   ⇒ 先填 path 会让 n08 在整个 SO-05/SO-06 期间一直红（V0265 的文件头就是这么写的）。
--   所以 V0265 只种「码可授 + 页面节点就位」，本迁移在前端页与路由都落地之后补这一刀。
--
-- # 二、本次落地的前端资产（顺序即施工顺序，不可交换）
--   ① `frontend/src/views/business/erp/sales-order/index.vue`
--      （列表 / 新建 / 编辑 / 提交 / 撤回 / 作废 / 关闭 / 详情抽屉 + 审批进度 FlowProgress）
--   ② `frontend/src/router/index.ts`：
--        path: 'business/erp/sales-order'  name: 'BusinessErpSalesOrder'
--        component: () => import('@/views/business/erp/sales-order/index.vue')
--        meta.permission: 'business:erp:sale-order:list'
--      n08 把路由表里的 `path: 'x'` 统一补成 `/x` 后与 `sys_menu.path` **整串比对**
--      ⇒ 本迁移写的 path 必须正好是 `/business/erp/sales-order`（多一段少一段都红）。
--   ③ 本迁移：把 …2651 的 path / component / is_visible 补成终态。
--
-- # 三、title 一并收敛成「销售订单」（照 V0263 的先例）
--   侧边栏渲染的是 `sys_menu.title`，V0265 种的长句（「销售订单（销售链单据：单号 …」）
--   会在侧边栏整段铺开。客户档案卡当时就是 V0262 长句 → V0263 收敛，这里一次做完：
--   终态 title = `销售订单`，一句话说明留在本文件与代码注释里，不塞进导航。
--
-- # 四、幂等与回滚
--   幂等：UPDATE 三条都带 `IS DISTINCT FROM`，在已补齐的库上重放 = 0 行更新；
--        末尾 DO 块自检（节点存在 / menu_type=2 / 三列终态 / 3 个按钮仍挂在页面下），
--        任一不符 → 迁移失败（宁可启动失败，也不要「菜单点了 404」上线）。
--   回滚：`V0282__sales_order_menu_page.down.sql` 把三列还原成 V0265 的 NULL / NULL / FALSE，
--        并断言「页面下 3 个按钮仍在」—— 回滚只关页面可见性，不删任何权限码（那是 V0265 的事）。
--   不写 `sys_role_menu`：授哪些角色看销售订单是管理决策（与 V0250 / V0261 / V0265 同口径）。
-- ============================================================================

DO $$
DECLARE
    v_page       uuid := '20000000-0000-0000-0000-000000002651'::uuid;
    v_path       text := '/business/erp/sales-order';
    v_component  text := 'business/erp/sales-order/index';
    v_title      text := '销售订单';
    v_type       int;
    v_code       text;
    v_children   int;
    n_updated    int := 0;
    n_bad        int;
BEGIN
    -- ---------- 0) 前置：页面节点必须存在、是页面（menu_type=2）、码未被换掉 ----------
    SELECT menu_type, auth_code INTO v_type, v_code
      FROM sys_menu
     WHERE id = v_page AND deleted_at IS NULL;

    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0282 前置失败：销售订单页面节点 …2651 不存在或已软删（V0265 没跑或已被回滚）';
    END IF;
    IF v_type <> 2 THEN
        RAISE EXCEPTION 'V0282 前置失败：…2651 的 menu_type = %（应为 2 页面节点）', v_type;
    END IF;
    IF v_code IS DISTINCT FROM 'business:erp:sale-order:list' THEN
        RAISE EXCEPTION 'V0282 前置失败：…2651 的 auth_code = %（应为 business:erp:sale-order:list）', v_code;
    END IF;

    -- ---------- 1) 补 path（幂等） ----------
    UPDATE sys_menu
       SET path = v_path, updated_at = now()
     WHERE id = v_page
       AND path IS DISTINCT FROM v_path;
    GET DIAGNOSTICS n_updated = ROW_COUNT;

    -- ---------- 2) 补 component（幂等） ----------
    UPDATE sys_menu
       SET component = v_component, updated_at = now()
     WHERE id = v_page
       AND component IS DISTINCT FROM v_component;

    -- ---------- 3) 开可见性 + 更新说明文案（幂等） ----------
    UPDATE sys_menu
       SET is_visible = TRUE,
           title = v_title,
           updated_at = now()
     WHERE id = v_page
       AND (is_visible IS DISTINCT FROM TRUE OR title IS DISTINCT FROM v_title);

    -- ---------- 4) 施工后断言 ----------
    -- 4.1 三列必须是终态（任何库都必须成立）
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL
       AND (path      IS DISTINCT FROM v_path
         OR component IS DISTINCT FROM v_component
         OR is_visible IS DISTINCT FROM TRUE);
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0282 断言 A 失败：销售订单页的 path/component/is_visible 未补成终态';
    END IF;

    -- 4.2 结构不能在这条 UPDATE 里丢：3 个按钮仍挂在页面下（V0265 的成果必须还在）
    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = v_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_children <> 3 THEN
        RAISE EXCEPTION 'V0282 断言 B 失败：销售订单页下应有 3 个按钮，实际 % 个（父级关系被破坏）', v_children;
    END IF;

    -- 4.3 提示：n08 是「库 × 前端路由」的联合校验，本迁移只能保证库这一侧；
    --     路由那一侧由 CI 的 `cargo test --lib system::menu`（真库档）守。
    RAISE NOTICE 'V0282 完成：销售订单页已开（path=% component=%），本次改 path % 行；3 个按钮父级完好',
        v_path, v_component, n_updated;
END $$;
