-- ======================================================
-- V0293：收款单接口的 5 个权限码（SO-17 后端 API 的菜单承载）
--
-- ## 为什么本卡要带迁移
--
-- `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
-- 里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**
-- ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
--
-- SO-17 的 5 个接口 ↔ 5 个码（与 `receipt.rs` 里的 `PERM_*` 常量逐一对应，
-- 与 `#[nik::permission(permission = "…")]` 逐字一致，差一个字 = 接口永远 403）：
--
--     business:erp:receipt:list      列表（GET  /receipt/page）
--     business:erp:receipt:detail    详情（GET  /receipt/{id}）
--     business:erp:receipt:create    登记收款（POST /receipt）
--     business:erp:receipt:allocate  核销到应收（POST /receipt/{id}/allocate）
--     business:erp:receipt:status    状态迁移（PATCH /receipt/{id}/status）
--
-- ## 结构：一个页面节点 + 4 个按钮
--
--   收款单（menu_type=2，auth_code = …:list，id …2890）
--     ├─ …:detail    （menu_type=3，…2891）
--     ├─ …:create    （menu_type=3，…2892）
--     ├─ …:allocate  （menu_type=3，…2893）
--     └─ …:status    （menu_type=3，…2894）
--
-- ## `path` / `component` 故意留 NULL，`is_visible = FALSE`（与 V0261 / V0284 / V0288 同一套口子）
--
-- 前端页面 `frontend/src/views/business/erp/receipt/` 与 `router/index.ts` 的路由
-- 由**前端卡（SO-18）**落地。`system::menu::tests::n08`（「menu_type=2 的 path 必须能在
-- 前端路由里解析」）是真库档硬门禁，提前填 `/business/erp/receipt` 会让 n08 在前端卡
-- 落地之前一直红。前端路由落地后一条 UPDATE 收尾（照 V0291 的手法）：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/receipt',
--            component = 'business/erp/receipt/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000002890'::uuid;
--
-- ## 不静默扩权
--
-- 本迁移**不写** `sys_role_menu`：收款单授给哪些角色（销售 / 财务是否只读）是管理决策，
-- 不属于迁移该替人做的事 —— 与 V0250 / V0261 / V0284 / V0288 同口径。
-- 现状：只有 SUPER_ADMIN 能调（短路），SO-17 的 curl 验收就是用它。
-- 因为一行授权都没有，`orphan_button_gate`（闸⑦）也不会把 4 个按钮判成孤儿。
--
-- ## 幂等与回滚
--
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检 5 个码与父子关系，
-- 任一不符 → 迁移失败（宁可启动失败，也不要「菜单半套」上线）。
-- 回滚：V0293__sales_receipt_permissions.down.sql 先删 `sys_role_menu` 授权行，再删 5 行菜单。
-- ======================================================

-- ---------- ① 页面节点（path/component 留 NULL，等前端路由卡落地后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002890'::uuid, '收款单', 2,
     'business:erp:receipt:list', NULL, NULL,
     NULL, 80, 1, 'wallet',
     '收款单（销售链：RC 取号 / 4 态 / 一单核销多张应收）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 四个动作按钮（挂在页面节点下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002891'::uuid, 'business:erp:receipt:detail', 3,
     'business:erp:receipt:detail', NULL,
     '20000000-0000-0000-0000-000000002890'::uuid, NULL,
     10, 1, NULL, '收款单详情（客户 / 凭证号 / 核销明细）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002892'::uuid, 'business:erp:receipt:create', 3,
     'business:erp:receipt:create', NULL,
     '20000000-0000-0000-0000-000000002890'::uuid, NULL,
     20, 1, NULL, '登记收款（RC 取号；金额 > 0；方式白名单）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002893'::uuid, 'business:erp:receipt:allocate', 3,
     'business:erp:receipt:allocate', NULL,
     '20000000-0000-0000-0000-000000002890'::uuid, NULL,
     30, 1, NULL, '核销到应收（一单多张，回写应收状态）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002894'::uuid, 'business:erp:receipt:status', 3,
     'business:erp:receipt:status', NULL,
     '20000000-0000-0000-0000-000000002890'::uuid, NULL,
     40, 1, NULL, '状态迁移（确认 / 过账生成凭证 / 作废）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 自检：5 个码齐全、4 个按钮挂在页面下、页面隐藏且未提前填 path ----------
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
              'business:erp:receipt:list',
              'business:erp:receipt:detail',
              'business:erp:receipt:create',
              'business:erp:receipt:allocate',
              'business:erp:receipt:status'
           ]) AS want
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_menu m
               WHERE m.auth_code = want AND m.deleted_at IS NULL
           );

    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0293 自检失败：权限码在 sys_menu 里缺承载：%', v_missing;
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000002890'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;

    IF v_children <> 4 THEN
        RAISE EXCEPTION 'V0293 自检失败：收款单页面下应有 4 个按钮，实际 % 个', v_children;
    END IF;

    SELECT is_visible, path INTO v_page_visible, v_page_path
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000002890'::uuid;

    IF v_page_visible IS NOT FALSE THEN
        RAISE EXCEPTION 'V0293 自检失败：收款单页 is_visible 必须仍是 FALSE（前端页由 SO-18 落地后再开）';
    END IF;

    IF v_page_path IS NOT NULL THEN
        RAISE EXCEPTION
            'V0293 自检失败：收款单页 path 必须留 NULL（当前 %）。'
            '前端路由落地后再 UPDATE 补 path/component/is_visible', v_page_path;
    END IF;

    RAISE NOTICE 'V0293：收款单 5 个权限码已种（页面节点隐藏且 path 待前端卡补），4 个按钮挂在页面下';
END $$;
