-- ======================================================
-- V0284：发货单接口的 3 个权限码（SO-09 后端 API 的菜单承载）
--
-- ## 为什么本卡要带迁移
--
-- `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
-- 里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**
-- ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
--
-- SO-09 新增 5 个接口（`shipment_handler.rs` 的 3 个码，`:list` 覆盖详情 + 分页 + 可发明细）：
--
--     business:erp:shipment:list     列表 / 详情 / 可发明细（GET /shippable/{so_id}）
--     business:erp:shipment:create   新建（可留空 items ⇒ 从订单带出未发完的行）
--     business:erp:shipment:status   状态流转（PATCH /status：提交 / 过账 / 作废）
--
-- 与 `shipment_handler.rs::SHIPMENT_PERMISSIONS` **逐字对应**（差一个字 = 接口永远 403）。
--
-- ## 结构：一个页面节点 + 2 个按钮
--
--   发货单（menu_type=2，auth_code = …:list，id …2841）
--     ├─ …:create（menu_type=3，…2842）
--     └─ …:status（menu_type=3，…2843）
--
-- ## `path` / `component` 故意留 NULL，`is_visible = FALSE`（与 V0261 / V0265 同一套口子）
--
-- 前端页面 `frontend/src/views/business/erp/shipment/index.vue` 与 `router/index.ts`
-- 的路由由**前端卡（SO-11）**落地。`system::menu::tests::n08`
-- （「menu_type=2 的 path 必须能在前端路由里解析」）是真库档硬门禁，
-- 提前填 `/business/erp/shipment` 会让 n08 在前端卡落地之前一直红。前端路由落地后一条 UPDATE 收尾：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/shipment',
--            component = 'business/erp/shipment/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000002841'::uuid;
--
-- ## 不静默扩权
--
-- 本迁移**不写** `sys_role_menu`：把「发货单」授给哪些角色是管理决策
-- （仓管 / 销售 / 财务是否只读），不属于迁移该替人做的决定 —— 与 V0250 / V0261 / V0265 同口径。
-- 现状：只有 SUPER_ADMIN 能调（短路），SO-09 的 curl 验收就是用它。
-- 因为一行授权都没有，`orphan_button_gate`（闸⑦）也不会把 2 个按钮判成孤儿。
--
-- ## 幂等与回滚
--
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检 3 个码与父子关系，
-- 任一不符 → 迁移失败（宁可启动失败，也不要「菜单半套」上线）。
-- 回滚：V0284__sales_shipment_permissions.down.sql 先删 `sys_role_menu` 授权行，再删 3 行菜单。
-- ======================================================

-- ---------- ① 页面节点（path/component 留 NULL，等前端路由卡落地后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002841'::uuid, '发货单', 2,
     'business:erp:shipment:list', NULL, NULL,
     NULL, 78, 1, 'send',
     '发货单（销售链单据：单号 SH-yyyyMMdd-000001 / 4 态状态机 / 从销售订单带出未发完的行）。path/component/可见性由前端卡落地后补',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 两个动作按钮（挂在页面节点下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002842'::uuid, 'business:erp:shipment:create', 3,
     'business:erp:shipment:create', NULL,
     '20000000-0000-0000-0000-000000002841'::uuid, NULL,
     10, 1, NULL, '新建发货单（取号 SH-yyyyMMdd-000001；items 留空 = 从订单带出全部未发完的行；超发 400 / 订单未审批 409）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002843'::uuid, 'business:erp:shipment:status', 3,
     'business:erp:shipment:status', NULL,
     '20000000-0000-0000-0000-000000002841'::uuid, NULL,
     20, 1, NULL, '发货单状态流转（提交待过账 / 过账（过账前校验库存充足）/ 作废，只走 4 条合法边）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 自检：3 个码齐全、2 个按钮挂在页面下、页面隐藏且未提前填 path ----------
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
              'business:erp:shipment:list',
              'business:erp:shipment:create',
              'business:erp:shipment:status'
           ]) AS want
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_menu m
               WHERE m.auth_code = want AND m.deleted_at IS NULL
           );

    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0284 自检失败：权限码在 sys_menu 里缺承载：%', v_missing;
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000002841'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;

    IF v_children <> 2 THEN
        RAISE EXCEPTION 'V0284 自检失败：发货单页下应有 2 个按钮，实际 % 个', v_children;
    END IF;

    SELECT is_visible, path INTO v_page_visible, v_page_path
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000002841'::uuid;

    IF v_page_visible IS NOT FALSE THEN
        RAISE EXCEPTION 'V0284 自检失败：发货单页 is_visible 必须仍是 FALSE（前端页由 SO-11 落地后再开）';
    END IF;

    IF v_page_path IS NOT NULL THEN
        RAISE EXCEPTION
            'V0284 自检失败：发货单页 path 必须留 NULL（当前 %）。'
            '前端路由落地后再 UPDATE 补 path/component/is_visible', v_page_path;
    END IF;

    RAISE NOTICE 'V0284：发货单 3 个权限码已种（页面节点隐藏且 path 待前端卡补），2 个按钮挂在页面下';
END $$;
