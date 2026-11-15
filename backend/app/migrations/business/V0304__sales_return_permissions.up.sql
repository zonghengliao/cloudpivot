-- ======================================================
-- V0304：销售退货单接口的 3 个权限码（SO-29 后端 API 的菜单承载）
--
-- ## 为什么本卡要带迁移
--
-- `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
-- 里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**
-- ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
--
-- SO-29 新增 5 个接口（`sales_return_handler.rs` 的 3 个码，`:list` 覆盖详情 + 分页 +
-- 可退明细）：
--
--     business:erp:sales-return:list     列表 / 详情 / 可退明细（GET /returnable/{shipment_id}）
--     business:erp:sales-return:create   新建（可留空 items ⇒ 从原发货单带出可退明细）
--     business:erp:sales-return:status   状态流转（PATCH /status：提交 / 批准 / 待过账 / 作废）
--
-- 与 `sales_return_handler.rs::SALES_RETURN_PERMISSIONS` **逐字对应**（差一个字 = 接口永远 403）。
--
-- ## 结构：一个页面节点 + 2 个按钮
--
--   销售退货单（menu_type=2，auth_code = …:list，id …3041）
--     ├─ …:create（menu_type=3，…3042）
--     └─ …:status（menu_type=3，…3043）
--
-- ## `path` / `component` 故意留 NULL，`is_visible = FALSE`（与 V0261 / V0265 / V0284 同一套口子）
--
-- 前端页面与 `router/index.ts` 的路由由**前端卡**落地（`system::menu::tests::n08`
-- 是真库档硬门禁：menu_type=2 的 path 必须能在前端路由里解析，提前填会让 n08 在前端卡
-- 落地之前一直红）。前端路由落地后一条 UPDATE 收尾：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/sales-return',
--            component = 'business/erp/sales-return/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000003041'::uuid;
--
-- ## 不静默扩权
--
-- 本迁移**不写** `sys_role_menu`：把「销售退货单」授给哪些角色是管理决策
-- （销售 / 仓管 / 财务是否只读），不属于迁移该替人做的决定 —— 与 V0250 / V0261 /
-- V0265 / V0284 同口径。现状：只有 SUPER_ADMIN 能调（短路），SO-29 的 curl 验收就是用它。
-- 因为一行授权都没有，`orphan_button_gate`（闸⑦）也不会把 2 个按钮判成孤儿。
--
-- ## 幂等与回滚
--
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检 3 个码与父子关系，
-- 任一不符 → 迁移失败（宁可启动失败，也不要「菜单半套」上线）。
-- 回滚：V0304__sales_return_permissions.down.sql 先删 `sys_role_menu` 授权行，再删 3 行菜单。
-- ======================================================

-- ---------- ① 页面节点（path/component 留 NULL，等前端路由卡落地后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003041'::uuid, '销售退货单', 2,
     'business:erp:sales-return:list', NULL, NULL,
     NULL, 79, 1, 'rollback',
     '销售退货单（销售链单据：单号 SR-yyyyMMdd-000001 / 6 态状态机 / 从原发货单带出可退明细 / 超退 400）。path/component/可见性由前端卡落地后补',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 两个动作按钮（挂在页面节点下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003042'::uuid, 'business:erp:sales-return:create', 3,
     'business:erp:sales-return:create', NULL,
     '20000000-0000-0000-0000-000000003041'::uuid, NULL,
     10, 1, NULL, '新建销售退货单（取号 SR-yyyyMMdd-000001；items 留空 = 从原发货单带出可退明细；超退 400 / 发货单未过账 409）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000003043'::uuid, 'business:erp:sales-return:status', 3,
     'business:erp:sales-return:status', NULL,
     '20000000-0000-0000-0000-000000003041'::uuid, NULL,
     20, 1, NULL, '销售退货单状态流转（提交 / 批准 / 待过账 / 作废，只走 7 条合法边；POSTED 由 SO-30 过账卡解锁）',
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
              'business:erp:sales-return:list',
              'business:erp:sales-return:create',
              'business:erp:sales-return:status'
           ]) AS want
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_menu m
               WHERE m.auth_code = want AND m.deleted_at IS NULL
           );

    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0304 自检失败：权限码在 sys_menu 里缺承载：%', v_missing;
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000003041'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;

    IF v_children <> 2 THEN
        RAISE EXCEPTION 'V0304 自检失败：销售退货单页下应有 2 个按钮，实际 % 个', v_children;
    END IF;

    SELECT is_visible, path INTO v_page_visible, v_page_path
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000003041'::uuid;

    IF v_page_visible IS NOT FALSE THEN
        RAISE EXCEPTION 'V0304 自检失败：销售退货单页 is_visible 必须仍是 FALSE（前端页落地后再开）';
    END IF;

    IF v_page_path IS NOT NULL THEN
        RAISE EXCEPTION 'V0304 自检失败：path 必须留 NULL（提前填会让 system::menu::tests::n08 在前端路由落地前一直红）';
    END IF;

    RAISE NOTICE 'V0304 完成：sales-return 3 个权限码已种入 sys_menu（1 页面 + 2 按钮，均隐藏）';
END $$;
