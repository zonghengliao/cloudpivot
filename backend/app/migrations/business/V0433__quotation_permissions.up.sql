-- ============================================================================
-- V0433__quotation_permissions（M3-02 · 报价单 5 个权限码 + 菜单承载 · 2026-11-17）
--
-- 卡：t_0750ad31（M3-02【M3·销售】报价单：CRUD + 状态机 草稿→已发→已接受/已拒绝）
-- 前置：V0432__sales_quotation（biz_quotation / biz_quotation_line / biz_seq(QT)）
--
-- ── 为什么本卡必须带迁移 ────────────────────────────────────────────────────
-- `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
-- 里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**
-- ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
-- M3-01 只建表、不种码（AGENTS.md 约定 7：码要跟接口一起种）—— 本卡出接口，所以种在
-- 这里，与 `quotation_handler::QUOTATION_PERMISSIONS` **逐字对应**（差一个字 = 接口永远 403）。
--
-- ── 5 个码 / 1 个页面节点 + 4 个按钮 ────────────────────────────────────────
--
--   报价单（menu_type=2，auth_code = business:erp:quotation:list，id …4331）
--     ├─ business:erp:quotation:create  新建（id …4332）
--     ├─ business:erp:quotation:update  编辑（只有 DRAFT）（id …4333）
--     ├─ business:erp:quotation:delete  删除（只有 DRAFT / CANCELLED）（id …4334）
--     └─ business:erp:quotation:status  状态流转（6 条合法边）（id …4335）
--
-- `:list` 既挂在页面节点上，也是「详情 + 分页」两个接口的码 —— 与销售订单
-- （V0265 `business:erp:sale-order:list`）完全同型：ERP 域尚未做「页面码 / 接口码解耦」
-- （AGENTS.md 约定 14 的过渡期条款，HR 是试点）。
--
-- ── title 口径（约定 12）─────────────────────────────────────────────────────
-- `sys_menu` **没有** `description` 列（实测 21 列，无此列），所以：
--   · 页面行（menu_type=2）的 `title` 是侧栏显示名 ⇒ **极短**（'报价单'，3 字），
--     功能说明只能留在本文件的注释里 + M3-06 的前端页面；
--   · 按钮行（menu_type=3）的 `title` 不进侧栏（menu_invariants ⑥ 只管 1/2），
--     按 V0265 的既定写法放一句能看懂的功能说明。
--
-- ── 挂载父节点 / 排序 ───────────────────────────────────────────────────────
-- 父 = 销售管理（`20000000-0000-0000-0000-000000002701`，path `/business/erp/sales-mgmt`）。
-- 同级已有 sort：10 客户档案 / 20 销售订单 / 30 发货单 / … / 92 汇率管理。
-- 报价单是「报价 → 订单」的前一手，取 **15**（落在 客户档案 与 销售订单 之间），
-- 与约定 12 的「同父 sort 必须唯一、插空用中间值」一致。
--
-- ── `path` / `component` 留 NULL、`is_visible = FALSE`（与 V0265 同一套口子）────
-- 前端页面 `/business/erp/quotation` 由**前端卡 M3-06** 落地。`system::menu::tests::n08`
-- （「menu_type=2 的 path 必须能在前端路由里解析」）是真库硬门禁，提前填 path 会让 n08
-- 在前端卡落地之前一直红。前端路由落地后**一条 UPDATE 收尾**：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/quotation',
--            component = 'business/erp/quotation/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000004331'::uuid;
--
-- ── 不静默扩权 ──────────────────────────────────────────────────────────────
-- 本迁移**不写** `sys_role_menu`：把报价单授给哪些角色（销售员 / 销售经理是否只读）
-- 是管理决策，不属于迁移该替人做的决定 —— 与 V0250 / V0261 / V0265 同口径。
-- 因为一行授权都没有，`orphan_button_gate`（闸⑦）也不会把这 4 个按钮判成孤儿。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────────
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；末尾 DO 块自检 5 个码与父子关系，
-- 任一不符 → 迁移失败（宁可启动失败，也不要「菜单半套」上线）。
-- down 先删 `sys_role_menu` 授权行（本迁移不写，但别处可能已授权），再删 5 行菜单。
--
-- ── 号段（开工实测复核）────────────────────────────────────────────────────
-- 写本文件时 backend/app/migrations/business/ 最大号 = V0432（M3-01 报价单表结构）。
-- 并行卡会抢号，**提交前**再 `ls backend/app/migrations/business | grep -oE 'V[0-9]{4}' | sort -u | tail -3` 复核。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：报价单表（V0432）与销售管理目录必须在
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_quotation') IS NULL THEN
        RAISE EXCEPTION 'V0433: biz_quotation 不存在，请先应用 V0432（销售报价单表结构）';
    END IF;
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0433: sys_menu 不存在（系统基线迁移没跑到）';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = '20000000-0000-0000-0000-000000002701'::uuid
           AND deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'V0433: 销售管理目录（…2701）不存在，请先应用 V0267（销售菜单）';
    END IF;
    RAISE NOTICE 'V0433: 前置齐备，开始种报价单 5 个权限码';
END $$;

-- ---------------------------------------------------------------------------
-- ② 页面节点（path/component 留 NULL，前端卡 M3-06 落地后再补）
--    功能说明（不进库，sys_menu 无 description 列）：
--      销售报价单：单号 QT-yyyyMMdd-000001；状态 DRAFT → SENT → ACCEPTED / REJECTED，
--      另有三条 → CANCELLED（撤回 / 客户反悔）；「已过期」按 valid_until < 今天现算。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004331'::uuid, '报价单', 2,
     'business:erp:quotation:list', NULL,
     '20000000-0000-0000-0000-000000002701'::uuid, NULL,
     15, 1, 'bill', '报价单',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ③ 四个动作按钮（挂在页面节点下）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004332'::uuid, 'business:erp:quotation:create', 3,
     'business:erp:quotation:create', NULL,
     '20000000-0000-0000-0000-000000004331'::uuid, NULL,
     10, 1, NULL, '新建报价单（取号 QT-yyyyMMdd-000001；客户须 ACTIVE、至少 1 行明细、有效期不早于报价日）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000004333'::uuid, 'business:erp:quotation:update', 3,
     'business:erp:quotation:update', NULL,
     '20000000-0000-0000-0000-000000004331'::uuid, NULL,
     20, 1, NULL, '编辑报价单（只有 DRAFT 能改；已发出的改价要开新单）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000004334'::uuid, 'business:erp:quotation:delete', 3,
     'business:erp:quotation:delete', NULL,
     '20000000-0000-0000-0000-000000004331'::uuid, NULL,
     30, 1, NULL, '删除报价单（软删，只有 DRAFT / CANCELLED 能删）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000004335'::uuid, 'business:erp:quotation:status', 3,
     'business:erp:quotation:status', NULL,
     '20000000-0000-0000-0000-000000004331'::uuid, NULL,
     40, 1, NULL, '状态流转（只走 QUOTATION_EDGES 的 6 条边；客户拒绝与已发出后作废必须给原因）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ④ 自检：5 个码齐全、4 个按钮挂在页面下、页面隐藏且未提前填 path、排序不撞车
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_missing text;
    v_children integer;
    v_page_visible boolean;
    v_page_path text;
    v_page_parent uuid;
    v_page_sort integer;
    v_clash integer;
BEGIN
    SELECT string_agg(want, ', ')
      INTO v_missing
      FROM unnest(ARRAY[
              'business:erp:quotation:list',
              'business:erp:quotation:create',
              'business:erp:quotation:update',
              'business:erp:quotation:delete',
              'business:erp:quotation:status'
           ]) AS want
     WHERE NOT EXISTS (
              SELECT 1 FROM sys_menu m
               WHERE m.auth_code = want AND m.deleted_at IS NULL
           );
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0433 自检失败：权限码在 sys_menu 里缺承载：%', v_missing;
    END IF;

    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000004331'::uuid
       AND menu_type = 3
       AND deleted_at IS NULL;
    IF v_children <> 4 THEN
        RAISE EXCEPTION 'V0433 自检失败：报价单页下应有 4 个按钮，实际 % 个', v_children;
    END IF;

    SELECT is_visible, path, parent_id, sort_order
      INTO v_page_visible, v_page_path, v_page_parent, v_page_sort
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004331'::uuid;
    IF v_page_visible IS NOT FALSE THEN
        RAISE EXCEPTION 'V0433 自检失败：报价单页 is_visible 必须仍是 FALSE（前端页由 M3-06 落地后再开）';
    END IF;
    IF v_page_path IS NOT NULL THEN
        RAISE EXCEPTION
            'V0433 自检失败：报价单页 path 必须留 NULL（当前 %）。前端路由落地后再 UPDATE 补 path/component/is_visible',
            v_page_path;
    END IF;
    IF v_page_parent IS DISTINCT FROM '20000000-0000-0000-0000-000000002701'::uuid THEN
        RAISE EXCEPTION 'V0433 自检失败：报价单页必须挂在销售管理目录（…2701）下，实际 %', v_page_parent;
    END IF;
    IF v_page_sort <> 15 THEN
        RAISE EXCEPTION 'V0433 自检失败：报价单页 sort_order 应为 15，实际 %', v_page_sort;
    END IF;

    -- 同父 menu_type=2 的 sort 不许重复（约定 12 / menu_invariants ⑦）
    SELECT count(*) INTO v_clash
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000002701'::uuid
       AND menu_type = 2
       AND deleted_at IS NULL
       AND sort_order = 15
       AND id <> '20000000-0000-0000-0000-000000004331'::uuid;
    IF v_clash <> 0 THEN
        RAISE EXCEPTION 'V0433 自检失败：销售管理下已存在 sort_order = 15 的页面（% 行），必须换一个空位', v_clash;
    END IF;

    -- 按钮 name 必须唯一（角色授权树按 name 定位）
    SELECT count(*) INTO v_clash
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND name = 'business:erp:quotation:create';
    IF v_clash <> 1 THEN
        RAISE EXCEPTION 'V0433 自检失败：按钮 name business:erp:quotation:create 应恰好 1 行，实际 %', v_clash;
    END IF;

    -- 页面 title 必须是短菜单名（约定 12：2-6 个字，硬顶 12）
    IF (SELECT length(title) FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000004331'::uuid) > 12 THEN
        RAISE EXCEPTION 'V0433 自检失败：报价单页 title 超过 12 字（约定 12：title 是侧栏显示名）';
    END IF;

    RAISE NOTICE 'V0433：报价单 5 个权限码已种（页面隐藏且 path 待 M3-06 补），4 个按钮挂在页面下，sort=15';
END $$;
