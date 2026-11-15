-- ======================================================
-- V0265：销售订单接口的 4 个权限码（SO-05 后端 API 的菜单承载）
--
-- ## 为什么本卡要带迁移
--
-- `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
-- 里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**
-- ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
--
-- SO-05 新增了 5 个接口（`sales_order_handler.rs` 的 4 个码，`:list` 覆盖详情 + 分页）：
--
--     business:erp:sale-order:list     列表 / 详情
--     business:erp:sale-order:create   新建
--     business:erp:sale-order:update   编辑（只有 DRAFT）
--     business:erp:sale-order:status   状态流转（PATCH /status）
--
-- 与 `sales_order_handler.rs::SALES_ORDER_PERMISSIONS` **逐字对应**（差一个字 = 接口永远 403）。
--
-- ## 结构：一个页面节点 + 3 个按钮
--
--   销售订单（menu_type=2，auth_code = …:list，id …2651）
--     ├─ …:create（menu_type=3，…2652）
--     ├─ …:update（menu_type=3，…2653）
--     └─ …:status（menu_type=3，…2654）
--
-- ## `path` / `component` 故意留 NULL，`is_visible = FALSE`（与 V0261 同一套口子）
--
-- 前端页面 `frontend/src/views/business/erp/sales-order/index.vue` 与
-- `router/index.ts` 的路由由**前端卡**落地。`system::menu::tests::n08`
-- （「menu_type=2 的 path 必须能在前端路由里解析」）是真库档硬门禁，
-- 提前填 `/business/erp/sales-order` 会让 n08 在前端卡落地之前一直红
-- （V0261 实测复现过：客户档案卡当时就是这条）。
-- 前端路由落地后**一条 UPDATE 收尾**：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/sales-order',
--            component = 'business/erp/sales-order/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000002651'::uuid;
--
-- ## 不静默扩权
--
-- 本迁移**不写** `sys_role_menu`：把「销售订单」授给哪些角色是管理决策
-- （销售员 / 销售经理 / 财务是否只读），不属于迁移该替人做的决定 —— 与 V0250 / V0261 同口径。
-- 现状：只有 SUPER_ADMIN 能调（短路），SO-05 的 curl 验收就是用它。
-- 因为一行授权都没有，`orphan_button_gate`（闸⑦）也不会把 3 个按钮判成孤儿。
--
-- ## 幂等与回滚
--
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检 4 个码与父子关系，
-- 任一不符 → 迁移失败（宁可启动失败，也不要「菜单半套」上线）。
-- 回滚：V0265__sales_order_permissions.down.sql 先删 `sys_role_menu` 授权行，再删 4 行菜单。
-- ======================================================

-- ---------- ① 页面节点（path/component 留 NULL，等前端路由卡落地后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002651'::uuid, '销售订单', 2,
     'business:erp:sale-order:list', NULL, NULL,
     NULL, 77, 1, 'cart',
     '销售订单（销售链单据：单号 SO-yyyyMMdd-000001 / 7 态状态机 / 明细发货进度）。path/component/可见性由前端卡落地后补',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 三个动作按钮（挂在页面节点下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002652'::uuid, 'business:erp:sale-order:create', 3,
     'business:erp:sale-order:create', NULL,
     '20000000-0000-0000-0000-000000002651'::uuid, NULL,
     10, 1, NULL, '新建销售订单（取号 SO-yyyyMMdd-000001，客户必须存在且未冻结，至少 1 行明细）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002653'::uuid, 'business:erp:sale-order:update', 3,
     'business:erp:sale-order:update', NULL,
     '20000000-0000-0000-0000-000000002651'::uuid, NULL,
     20, 1, NULL, '编辑销售订单（只有 DRAFT 能改，明细整单替换，金额由服务端重算）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002654'::uuid, 'business:erp:sale-order:status', 3,
     'business:erp:sale-order:status', NULL,
     '20000000-0000-0000-0000-000000002651'::uuid, NULL,
     30, 1, NULL, '销售订单状态流转（只走 LEGAL_EDGES 的 10 条合法边，非法迁移 409）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 自检：4 个码齐全、3 个按钮挂在页面下、页面隐藏且未提前填 path ----------
DO $$
DECLARE
    v_missing text;
    v_children integer;
    v_page_visible boolean;
    v_page_path text;
BEGIN
    SELECT string_agg(want, ', ')
      INTO v_missing
      FROM unnest(ARRAY[
              'business:erp:sale-order:list',
              'business:erp:sale-order:create',
              'business:erp:sale-order:update',
              'business:erp:sale-order:status'
           ]) AS want
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_menu m
               WHERE m.auth_code = want AND m.deleted_at IS NULL
           );

    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0265 自检失败：权限码在 sys_menu 里缺承载：%', v_missing;
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000002651'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;

    IF v_children <> 3 THEN
        RAISE EXCEPTION 'V0265 自检失败：销售订单页下应有 3 个按钮，实际 % 个', v_children;
    END IF;

    SELECT is_visible, path INTO v_page_visible, v_page_path
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000002651'::uuid;

    IF v_page_visible IS NOT FALSE THEN
        RAISE EXCEPTION 'V0265 自检失败：销售订单页 is_visible 必须仍是 FALSE（前端页由前端卡落地后再开）';
    END IF;

    IF v_page_path IS NOT NULL THEN
        RAISE EXCEPTION
            'V0265 自检失败：销售订单页 path 必须留 NULL（当前 %）。'
            '前端路由落地后再 UPDATE 补 path/component/is_visible', v_page_path;
    END IF;

    RAISE NOTICE 'V0265：销售订单 4 个权限码已种（页面节点隐藏且 path 待前端卡补），3 个按钮挂在页面下';
END $$;
