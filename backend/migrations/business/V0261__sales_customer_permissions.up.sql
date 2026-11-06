-- ======================================================
-- V0261：客户档案接口的 4 个权限码（SO-02 后端 API 的菜单承载）
--
-- ## 为什么本卡要带迁移（不是「顺手多种几条菜单」）
--
-- `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
-- 里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**（管理面没有可勾的项）
-- ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
--
-- SO-02 新增了 5 个接口（`customer_handler.rs` 的 4 个码，`:list` 覆盖详情 + 分页）：
--
--     business:erp:customer:list     列表 / 详情
--     business:erp:customer:create   新建
--     business:erp:customer:update   编辑
--     business:erp:customer:delete   软删
--
-- 与 `customer_handler.rs::CUSTOMER_PERMISSIONS` **逐字对应**（差一个字 = 接口永远 403）。
--
-- ## 结构：一个页面节点 + 3 个按钮
--
--   客户档案（menu_type=2，auth_code = …:list，id …2601）
--     ├─ …:create（menu_type=3，…2602）
--     ├─ …:update（menu_type=3，…2603）
--     └─ …:delete（menu_type=3，…2604）
--
-- 父级挂**顶层页面**（`parent_id IS NULL`），与同族的「合同管理」（`…-000000001100`）一致：
-- 销售链目前没有独立的顶级目录，自己造一个「销售管理」目录会把 SO-03 的侧边栏归属决策
-- 提前替人做掉（那属于前端卡）。
--
-- ## `path` / `component` 故意留 NULL，`is_visible = FALSE`（**别顺手填上**）
--
-- 前端页面 `frontend/src/views/business/erp/customer/index.vue` 与
-- `router/index.ts` 的路由由 **SO-03** 落地。`system::menu::tests::n08`
-- （「menu_type=2 的 path 必须能在前端路由里解析」）是**真库档硬门禁**，
-- 现在就把 `/business/erp/customer` 填进来，在 SO-03 落地之前该用例必红
-- —— 本卡实测复现过：
--
--     以下菜单前端未注册路由，点进去必然 404：
--     客户档案 (/business/erp/customer, component=Some("business/erp/customer/index"))
--
-- 因此本迁移只交付「**码可授 + 页面节点就位**」这件事（`n08` 只看
-- `path IS NOT NULL AND path <> ''` 的行，NULL 跳过；按钮挂在页面下，
-- 不需要 SO-03 再做 reparent —— 这正是 V0259「先有按钮、后补父页」那个病根的反面）。
--
-- SO-03 落地前端路由后，**一条 UPDATE 收尾**（写进它的卡面 handoff）：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/customer',
--            component = 'business/erp/customer/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000002601'::uuid;
--
-- ## 不静默扩权
--
-- 本迁移**不写** `sys_role_menu`：把「客户档案」授给哪些角色是管理决策（销售员 / 销售经理 /
-- 财务是否只读），不属于迁移该替人做的决定 —— 与 V0250 的口径一致。
-- 现状：只有 SUPER_ADMIN 能调（短路），SO-02 的 curl 验收就是用它。
-- 因为一行授权都没有，`orphan_button_gate`（闸⑦）也不会把 3 个按钮判成孤儿（实测 0 对）。
--
-- ## 幂等与回滚
--
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检 4 个码与父子关系，
-- 任一不符 → 迁移失败（宁可启动失败，也不要「菜单半套」上线）。
-- 回滚：V0261__sales_customer_permissions.down.sql 先删 `sys_role_menu` 授权行，再删 4 行菜单。
-- ======================================================

-- ---------- ① 页面节点（path/component 留 NULL，等 SO-03 落地路由后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002601'::uuid, '客户档案', 2,
     'business:erp:customer:list', NULL, NULL,
     NULL, 76, 1, 'user',
     '客户档案（销售链交易对手主档：编码 / 名称 / 信用额度 / 状态）。path/component/可见性由 SO-03 落地前端页后补',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 三个动作按钮（挂在页面节点下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002602'::uuid, 'business:erp:customer:create', 3,
     'business:erp:customer:create', NULL,
     '20000000-0000-0000-0000-000000002601'::uuid, NULL,
     10, 1, NULL, '新建客户（编码未删内唯一，信用额度 >= 0）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002603'::uuid, 'business:erp:customer:update', 3,
     'business:erp:customer:update', NULL,
     '20000000-0000-0000-0000-000000002601'::uuid, NULL,
     20, 1, NULL, '编辑客户（部分更新；信用额度 >= 0）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002604'::uuid, 'business:erp:customer:delete', 3,
     'business:erp:customer:delete', NULL,
     '20000000-0000-0000-0000-000000002601'::uuid, NULL,
     30, 1, NULL, '删除客户（软删；编码可复用）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 自检：4 个码齐全、3 个按钮挂在页面下、页面仍隐藏且未提前填 path ----------
DO $$
DECLARE
    v_missing text;
    v_children integer;
    v_page_visible boolean;
    v_page_path text;
BEGIN
    -- 4 个码必须都能在 sys_menu 里查到（软删不算承载）
    SELECT string_agg(want, ', ')
      INTO v_missing
      FROM unnest(ARRAY[
              'business:erp:customer:list',
              'business:erp:customer:create',
              'business:erp:customer:update',
              'business:erp:customer:delete'
           ]) AS want
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_menu m
               WHERE m.auth_code = want AND m.deleted_at IS NULL
           );

    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0261 自检失败：权限码在 sys_menu 里缺承载：%', v_missing;
    END IF;

    -- 3 个按钮必须挂在客户档案页下（否则是「孤儿按钮」，见 orphan_button_gate）
    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000002601'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;

    IF v_children <> 3 THEN
        RAISE EXCEPTION 'V0261 自检失败：客户档案页下应有 3 个按钮，实际 % 个', v_children;
    END IF;

    -- 页面必须仍是隐藏的：前端页由 SO-03 落地，提前可见会点进不存在的组件
    SELECT is_visible, path INTO v_page_visible, v_page_path
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000002601'::uuid;

    IF v_page_visible IS NOT FALSE THEN
        RAISE EXCEPTION 'V0261 自检失败：客户档案页 is_visible 必须仍是 FALSE（前端页由 SO-03 落地后再开）';
    END IF;

    -- path 不能提前填：填了 n08（菜单路径必须能在前端路由解析）在 SO-03 之前必红
    IF v_page_path IS NOT NULL THEN
        RAISE EXCEPTION
            'V0261 自检失败：客户档案页 path 必须留 NULL（当前 %）。'
            '前端路由由 SO-03 落地后再 UPDATE 补 path/component/is_visible', v_page_path;
    END IF;

    RAISE NOTICE 'V0261：客户档案 4 个权限码已种（页面节点隐藏且 path 待 SO-03 补），3 个按钮挂在页面下';
END $$;
