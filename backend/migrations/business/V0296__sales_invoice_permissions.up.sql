-- ======================================================
-- V0296：销售发票接口的 4 个权限码（SO-20 后端 API 的菜单承载）
--
-- ## 为什么本卡要带迁移
--
-- `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
-- 里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**
-- ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
--
-- SO-20 的 4 个接口 ↔ 4 个码（与 `invoice.rs` 的 `PERM_*` 常量逐一对应，
-- 与 `#[nik::permission(permission = "…")]` 逐字一致，差一个字 = 接口永远 403）：
--
--     business:erp:invoice:list      列表（GET  /invoice/page）
--     business:erp:invoice:detail    详情（GET  /invoice/{id}）
--     business:erp:invoice:create    开票（POST /invoice）
--     business:erp:invoice:status    状态迁移（PATCH /invoice/{id}/status：ISSUED / POSTED / VOID）
--
-- ## 结构：一个页面节点 + 3 个按钮
--
--   销售发票（menu_type=2，auth_code = …:list，id …2895）
--     ├─ …:detail    （menu_type=3，…2896）
--     ├─ …:create    （menu_type=3，…2897）
--     └─ …:status    （menu_type=3，…2898）
--
-- ## `path` / `component` 故意留 NULL，`is_visible = FALSE`（与 V0261 / V0284 / V0288 / V0293 同一套口子）
--
-- 前端页面 `frontend/src/views/business/erp/invoice/` 与 `router/index.ts` 的路由
-- 由**前端卡（SO-21）**落地。`system::menu::tests::n08`（「menu_type=2 的 path 必须能在
-- 前端路由里解析」）是真库档硬门禁，提前填 `/business/erp/invoice` 会让 n08 在前端卡
-- 落地之前一直红。前端路由落地后一条 UPDATE 收尾（照 V0294 的手法）：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/invoice',
--            component = 'business/erp/invoice/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000002895'::uuid;
--
-- ## 不静默扩权
--
-- 本迁移**不写** `sys_role_menu`：发票授给哪些角色（销售 / 财务是否只读）是管理决策，
-- 不属于迁移该替人做的事 —— 与 V0250 / V0261 / V0284 / V0288 / V0293 同口径。
-- 现状：只有 SUPER_ADMIN 能调（短路），SO-20 的 curl 验收就是用它。
-- 因为一行授权都没有，`orphan_button_gate`（闸⑦）也不会把 3 个按钮判成孤儿。
--
-- ## 幂等与回滚
--
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检 4 个码与父子关系，
-- 任一不符 → 迁移失败（宁可启动失败，也不要「菜单半套」上线）。
-- 回滚：V0296__sales_invoice_permissions.down.sql 先删 `sys_role_menu` 授权行，再删 4 行菜单。
-- ======================================================

-- ---------- ① 页面节点（path/component 留 NULL，等前端路由卡落地后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002895'::uuid, '销售发票', 2,
     'business:erp:invoice:list', NULL, NULL,
     NULL, 85, 1, 'file-copy',
     '销售发票（销售链：IV 取号 / 4 态 / 开票红冲）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 三个动作按钮（挂在页面节点下） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000002896'::uuid, 'business:erp:invoice:detail', 3,
     'business:erp:invoice:detail', NULL,
     '20000000-0000-0000-0000-000000002895'::uuid, NULL,
     10, 1, NULL, '发票详情（客户 / 应收 / 凭证号 / 可开票额度）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002897'::uuid, 'business:erp:invoice:create', 3,
     'business:erp:invoice:create', NULL,
     '20000000-0000-0000-0000-000000002895'::uuid, NULL,
     20, 1, NULL, '开票（从应收带出金额 / 手工开票；金额不得超过应收）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000002898'::uuid, 'business:erp:invoice:status', 3,
     'business:erp:invoice:status', NULL,
     '20000000-0000-0000-0000-000000002895'::uuid, NULL,
     30, 1, NULL, '状态迁移（开票生成收入凭证 / 归档 / 作废红冲）',
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
              'business:erp:invoice:list',
              'business:erp:invoice:detail',
              'business:erp:invoice:create',
              'business:erp:invoice:status'
           ]) AS want
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_menu m
               WHERE m.auth_code = want AND m.deleted_at IS NULL
           );

    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0296 自检失败：权限码在 sys_menu 里缺承载：%', v_missing;
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000002895'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;

    IF v_children <> 3 THEN
        RAISE EXCEPTION 'V0296 自检失败：销售发票页面下应有 3 个按钮，实际 % 个', v_children;
    END IF;

    SELECT is_visible, path INTO v_page_visible, v_page_path
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000002895'::uuid;

    IF v_page_visible IS NOT FALSE THEN
        RAISE EXCEPTION 'V0296 自检失败：销售发票页 is_visible 必须仍是 FALSE（前端页由 SO-21 落地后再开）';
    END IF;

    IF v_page_path IS NOT NULL THEN
        RAISE EXCEPTION
            'V0296 自检失败：销售发票页 path 必须留 NULL（当前 %）。'
            '前端路由落地后再 UPDATE 补 path/component/is_visible', v_page_path;
    END IF;

    RAISE NOTICE 'V0296：销售发票 4 个权限码已种（页面节点隐藏且 path 待前端卡补），3 个按钮挂在页面下';
END $$;
