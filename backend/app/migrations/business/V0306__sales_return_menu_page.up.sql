-- ============================================================================
-- V0306：销售退货单页「落地收尾」—— 把 V0304 故意留空的 path / component / is_visible 补齐，
--        并把页面挂到「销售管理」目录下（sort 70，紧跟销售发票 60）
--
-- 依据：kanban 卡 t_f7b9e46c（SO-31 退货单前端页面），承接 t_126909b0（SO-30 退货过账）。
--       逐行镜像 V0297（SO-21 发票）/ V0294（SO-18 收款单）/ V0291（SO-15 应收台账）。
--
-- # 一、为什么分两步（V0304 留 NULL → 本迁移补全）
--   `system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router`（真库档硬门禁）要求
--   `menu_type = 2 AND path IS NOT NULL AND path <> ''` 的每一行 path 都必须能在
--   `frontend/src/router/index.ts` 解析到。V0304 只种权限码时前端页与路由都不存在
--   ⇒ 先填 path 会让 n08 在 SO-29/30 期间一直红（V0304 头注释原文：「前端路由落地后
--   一条 UPDATE 收尾」），所以本卡先落前端页 + 路由，再由本迁移补这一刀。
--
-- # 二、本次落地的前端资产（顺序即施工顺序，不可交换）
--   ① `frontend/src/api/business/erp/sales-return.ts`（类型 + 5 个接口函数，字段逐字抄 handler）
--   ② `frontend/src/views/business/erp/sales-return/sales-return-query.ts`（纯逻辑，单测钉住）
--   ③ `frontend/src/views/business/erp/sales-return/index.vue`
--      （列表 6 列 + 服务端分页 + 3 过滤 / 从发货单新建（带出可退明细）/ 提交·批准·待过账·
--       过账·作废 5 个按状态显隐的动作 / 详情抽屉 + 状态线 + FlowProgress 审批进度）
--   ④ `frontend/src/router/index.ts`：
--        path: 'business/erp/sales-return'  name: 'BusinessErpSalesReturn'
--        component: () => import('@/views/business/erp/sales-return/index.vue')
--        meta.permission: 'business:erp:sales-return:list'
--      n08 把路由表里的 `path: 'x'` 统一补成 `/x` 后与 `sys_menu.path` **整串比对**
--      ⇒ 本迁移写的 path 必须正好是 `/business/erp/sales-return`（多一段少一段都红）。
--   ⑤ 本迁移：补 path / component / is_visible，收敛标题成「销售退货单」，
--      并**归位到「销售管理」目录**（V0267 种的 …2701；V0304 建节点时 parent_id 留空，
--      不归位就会在侧边栏根级多出一条孤儿页）。
--
-- # 三、幂等与回滚
--   幂等：4 条 UPDATE 全带 `IS DISTINCT FROM`，在已补齐的库上重放 = 0 行更新；
--        末尾 DO 块自检（节点存在 / menu_type=2 / 四列终态 / 3 个按钮仍挂在页面下），
--        任一不符 → 迁移失败（宁可启动失败，也不要「菜单点了 404」上线）。
--   回滚：`V0306__sales_return_menu_page.down.sql` 把各列还原成 V0304 的
--        NULL / NULL / FALSE / 原长标题 / parent NULL / sort 79，并断言「页面下 2 个按钮仍在」
--        —— 回滚只关页面可见性与归位，不删任何权限码（那是 V0304 的事）。
--   不写 `sys_role_menu`：授哪些角色看退货单是管理决策（与 V0250 / V0261 / V0284 / V0304
--        同口径；现状只有 SUPER_ADMIN 可见可用）。
-- ============================================================================

DO $$
DECLARE
    v_page       uuid := '20000000-0000-0000-0000-000000003041'::uuid;
    v_parent     uuid := '20000000-0000-0000-0000-000000002701'::uuid;  -- 销售管理（V0267）
    v_path       text := '/business/erp/sales-return';
    v_component  text := 'business/erp/sales-return/index';
    v_title      text := '销售退货单';
    v_sort       int  := 70;
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
        RAISE EXCEPTION 'V0306 前置失败：销售退货单页面节点 …3041 不存在或已软删（V0304 没跑或已被回滚）';
    END IF;
    IF v_type <> 2 THEN
        RAISE EXCEPTION 'V0306 前置失败：…3041 的 menu_type = %（应为 2 页面节点）', v_type;
    END IF;
    IF v_code IS DISTINCT FROM 'business:erp:sales-return:list' THEN
        RAISE EXCEPTION 'V0306 前置失败：…3041 的 auth_code = %（应为 business:erp:sales-return:list）', v_code;
    END IF;

    -- ---------- 0.1) 前置：「销售管理」目录必须在（归位目标） ----------
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE id = v_parent AND menu_type = 1 AND deleted_at IS NULL;
    IF n_bad <> 1 THEN
        RAISE EXCEPTION 'V0306 前置失败：「销售管理」目录 …2701 不存在或不是目录节点（V0267 没跑？）';
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

    -- ---------- 3) 开可见性 + 收敛标题（幂等） ----------
    UPDATE sys_menu
       SET is_visible = TRUE,
           title = v_title,
           updated_at = now()
     WHERE id = v_page
       AND (is_visible IS DISTINCT FROM TRUE
            OR title IS DISTINCT FROM v_title);

    -- ---------- 4) 归位到「销售管理」目录（幂等；V0304 建节点时 parent_id 为空） ----------
    UPDATE sys_menu
       SET parent_id = v_parent,
           sort_order = v_sort,
           updated_at = now()
     WHERE id = v_page
       AND (parent_id IS DISTINCT FROM v_parent
            OR sort_order IS DISTINCT FROM v_sort);

    -- ---------- 5) 施工后断言 ----------
    -- 5.1 五列必须是终态（任何库都必须成立）
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL
       AND (path       IS DISTINCT FROM v_path
         OR component  IS DISTINCT FROM v_component
         OR is_visible IS DISTINCT FROM TRUE
         OR parent_id  IS DISTINCT FROM v_parent
         OR sort_order IS DISTINCT FROM v_sort);
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0306 断言 A 失败：销售退货单页的 path/component/is_visible/parent/sort 未补成终态';
    END IF;

    -- 5.2 结构不能在这条 UPDATE 里丢：2 个按钮仍挂在页面下（V0304 的成果必须还在）
    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = v_page AND menu_type = 3 AND deleted_at IS NULL;
    IF v_children <> 2 THEN
        RAISE EXCEPTION 'V0306 断言 B 失败：销售退货单页下应有 2 个按钮（create / status），实际 % 个（父级关系被破坏）', v_children;
    END IF;

    -- 5.3 n08 是「库 × 前端路由」的联合校验，本迁移只能保证库这一侧；
    --     路由那一侧由 `cargo test --lib system::menu`（真库档）与 `pnpm build` 守。
    RAISE NOTICE 'V0306 完成：销售退货单页已开（path=% component=% parent=销售管理 sort=%），本次改 path % 行；2 个按钮父级完好',
        v_path, v_component, v_sort, n_updated;
END $$;
