-- ======================================================
-- V0288：应收单接口的 5 个权限码（SO-13 后端 API 的菜单承载）
--
-- ## 为什么本卡要带迁移
--
-- `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
-- 里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**
-- ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
--
-- SO-13 新增 5 个接口，对应 5 个码（与 `receivable.rs` / `receivable_query.rs` /
-- `receivable_aging.rs` 里的 `permission = "…"` 逐字一致，差一个字 = 接口永远 403）：
--
--     business:erp:receivable:list       列表（GET /receivable/page）
--     business:erp:receivable:detail     详情（GET /receivable/{id}）
--     business:erp:receivable:aging      账龄分析（GET /receivable/aging）
--     business:erp:receivable:create     手工挂账（POST /receivable）
--     business:erp:receivable:write-off  坏账核销（PATCH /receivable/{id}/write-off）
--
-- ## 结构：一个页面节点 + 4 个按钮
--
--   应收账款（menu_type=2，auth_code = …:list，id …2880）
--     ├─ …:detail     （menu_type=3，…2881）
--     ├─ …:aging      （menu_type=3，…2882）
--     ├─ …:create     （menu_type=3，…2883）
--     └─ …:write-off  （menu_type=3，…2884）
--
-- ## `path` / `component` 故意留 NULL，`is_visible = FALSE`（与 V0261 / V0265 / V0284 同一套口子）
--
-- 前端页面 `frontend/src/views/business/erp/receivable/` 与 `router/index.ts`
-- 的路由由**前端卡（SO-15）**落地。`system::menu::tests::n08`
-- （「menu_type=2 的 path 必须能在前端路由里解析」）是真库档硬门禁，
-- 提前填 `/business/erp/receivable` 会让 n08 在前端卡落地之前一直红。前端路由落地后一条 UPDATE 收尾：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/receivable',
--            component = 'business/erp/receivable/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000002880'::uuid;
--
-- ## 不静默扩权
--
-- 本迁移**不写** `sys_role_menu`：应收台账授给哪些角色（销售 / 财务是否只读）
-- 是管理决策，不属于迁移该替人做的事 —— 与 V0250 / V0261 / V0265 / V0284 同口径。
-- 现状：只有 SUPER_ADMIN 能调（短路），SO-13 的 curl 验收就是用它。
-- 因为一行授权都没有，`orphan_button_gate`（闸⑦）也不会把 4 个按钮判成孤儿。
--
-- ## 幂等与回滚
--
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检 5 个码与父子关系，
-- 任一不符 → 迁移失败（宁可启动失败，也不要「菜单半套」上线）。
-- 回滚：V0288__sales_receivable_permissions.down.sql 先删 `sys_role_menu` 授权行，再删 5 行菜单。
-- ======================================================

-- ---------- ① 页面节点（path/component 留 NULL，等前端路由卡落地后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002880'::uuid, '应收账款', 2,
     'business:erp:receivable:list', NULL, NULL,
     NULL, 79, 1, 'money',
     '应收账款（销售链台账：单号 AR-yyyyMMdd-000001 / 4 态状态机 / 账龄分析）。path/component/可见性由前端卡 SO-15 落地后补',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 四个动作按钮（挂在页面节点下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002881'::uuid, 'business:erp:receivable:detail', 3,
     'business:erp:receivable:detail', NULL,
     '20000000-0000-0000-0000-000000002880'::uuid, NULL,
     10, 1, NULL, '应收单详情（含客户名 / 关联凭证号 / 是否逾期）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002882'::uuid, 'business:erp:receivable:aging', 3,
     'business:erp:receivable:aging', NULL,
     '20000000-0000-0000-0000-000000002880'::uuid, NULL,
     20, 1, NULL, '应收账龄分析（分档与应付账款账龄完全同口径：未到期 / 1-30 / 31-60 / 61-90 / 90 天以上）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002883'::uuid, 'business:erp:receivable:create', 3,
     'business:erp:receivable:create', NULL,
     '20000000-0000-0000-0000-000000002880'::uuid, NULL,
     30, 1, NULL, '手工挂账（取号 AR-yyyyMMdd-000001；source_type=MANUAL；amount 必填且 > 0；已结账期间 400）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002884'::uuid, 'business:erp:receivable:write-off', 3,
     'business:erp:receivable:write-off', NULL,
     '20000000-0000-0000-0000-000000002880'::uuid, NULL,
     40, 1, NULL, '坏账核销（OPEN/PARTIAL → WRITTEN_OFF，必须给原因；SETTLED/已核销 409）',
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
              'business:erp:receivable:list',
              'business:erp:receivable:detail',
              'business:erp:receivable:aging',
              'business:erp:receivable:create',
              'business:erp:receivable:write-off'
           ]) AS want
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_menu m
               WHERE m.auth_code = want AND m.deleted_at IS NULL
           );

    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0288 自检失败：权限码在 sys_menu 里缺承载：%', v_missing;
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000002880'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;

    IF v_children <> 4 THEN
        RAISE EXCEPTION 'V0288 自检失败：应收页面下应有 4 个按钮，实际 % 个', v_children;
    END IF;

    SELECT is_visible, path INTO v_page_visible, v_page_path
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000002880'::uuid;

    IF v_page_visible IS NOT FALSE THEN
        RAISE EXCEPTION 'V0288 自检失败：应收账款页 is_visible 必须仍是 FALSE（前端页由 SO-15 落地后再开）';
    END IF;

    IF v_page_path IS NOT NULL THEN
        RAISE EXCEPTION
            'V0288 自检失败：应收账款页 path 必须留 NULL（当前 %）。'
            '前端路由落地后再 UPDATE 补 path/component/is_visible', v_page_path;
    END IF;

    RAISE NOTICE 'V0288：应收 5 个权限码已种（页面节点隐藏且 path 待前端卡补），4 个按钮挂在页面下';
END $$;
