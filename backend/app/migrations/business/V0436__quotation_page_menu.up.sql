-- ============================================================================
-- V0436__quotation_page_menu（M3-06 · 报价单前端页收尾 · 2026-11-17）
--
-- 卡：t_3ae6db9c（M3-06【M3·销售】报价单：前端列表/详情页）
-- 前置：V0433__quotation_permissions（报价单 5 个码 + 页面节点 …4331，path 留 NULL、
--       is_visible = FALSE，注释里写清「前端路由落地后一条 UPDATE 收尾」）
--
-- ── 本迁移只做一件事 ────────────────────────────────────────────────────────
-- 把 V0433 种下的报价单页面节点（id …4331）从「只有权限码、没有页面」补成真页面：
--
--     path       = '/business/erp/quotation'
--     component  = 'business/erp/quotation/index'
--     is_visible = TRUE
--
-- 前端侧同一张卡落地：`frontend/src/views/business/erp/quotation/index.vue`
-- + `frontend/src/api/business/erp/quotation.ts`
-- + `frontend/src/router/index.ts` 注册 `path: 'business/erp/quotation'`。
--
-- ── 顺序为什么是「先路由、后填菜单」─────────────────────────────────────────
-- `system::menu::tests::n08`（真库硬门禁）要求 **每条 menu_type=2 且 path 非空的菜单，
-- 前端 `router/index.ts` 里必须有同名 path**；反过来没有「路由必须进菜单」的断言。
-- 所以：路由先合并进 main（本卡同一次提交），迁移才敢填 path ——
-- 反着做会让 n08 在两次部署之间一直红。
--
-- ── 不改的东西 ─────────────────────────────────────────────────────────────
-- - **不写 `sys_role_menu`**：把报价单授给哪些角色是管理决策（与 V0433 同口径）；
-- - **不动 6 个权限码 / 5 个按钮行**：V0433（4 个按钮）与 V0434（convert）已经种齐，本迁移连 title 都不碰；
-- - **不动 sort_order = 15**（销售管理下「客户档案 10 ← 报价单 15 → 销售订单 20」）。
--
-- ── 幂等 / 回滚 ────────────────────────────────────────────────────────────
-- 只为「还没补过 path」的行做 UPDATE（`WHERE path IS NULL`），重复执行影响 0 行；
-- down 把三列还原成 V0433 的形状（NULL / NULL / FALSE），**不删**任何菜单行、
-- 不删任何角色授权 —— 删码是 V0433 down 的事。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：页面节点在、且它是报价单页（防止 uuid 抄错改到别人的页面）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_page   uuid := '20000000-0000-0000-0000-000000004331'::uuid;
    v_parent uuid := '20000000-0000-0000-0000-000000002701'::uuid;
    v_row    record;
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0436: sys_menu 不存在（系统基线迁移没跑到）';
    END IF;

    SELECT id, name, auth_code, menu_type, parent_id, path, is_visible
      INTO v_row
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'V0436: 报价单页面节点 …4331 不存在或已软删，请先应用 V0433';
    END IF;
    IF v_row.menu_type <> 2 OR v_row.auth_code <> 'business:erp:quotation:list' THEN
        RAISE EXCEPTION
            'V0436: …4331 不是报价单页面节点（menu_type=%，auth_code=%)，拒绝改写',
            v_row.menu_type, v_row.auth_code;
    END IF;
    IF v_row.parent_id IS DISTINCT FROM v_parent THEN
        RAISE EXCEPTION 'V0436: 报价单页必须挂在销售管理目录（…2701）下，实际 %', v_row.parent_id;
    END IF;

    RAISE NOTICE 'V0436: 前置齐备（报价单页 当前 path=%，is_visible=%），开始补 path/component',
        coalesce(v_row.path, '<NULL>'), v_row.is_visible;
END $$;

-- ---------------------------------------------------------------------------
-- ② 补页面三列（只为还没补过的行做，重复执行 0 行）
-- ---------------------------------------------------------------------------
UPDATE sys_menu
   SET path       = '/business/erp/quotation',
       component  = 'business/erp/quotation/index',
       is_visible = TRUE,
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000004331'::uuid
   AND deleted_at IS NULL
   AND (path IS DISTINCT FROM '/business/erp/quotation'
     OR component IS DISTINCT FROM 'business/erp/quotation/index'
     OR is_visible IS DISTINCT FROM TRUE);

-- ---------------------------------------------------------------------------
-- ③ 自检：三列已落、path 全局唯一、按钮行还是 4 个、title 仍是短菜单名
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_page     uuid := '20000000-0000-0000-0000-000000004331'::uuid;
    v_path     text;
    v_component text;
    v_visible  boolean;
    v_title    text;
    v_dup      integer;
    v_children integer;
BEGIN
    SELECT path, component, is_visible, title
      INTO v_path, v_component, v_visible, v_title
      FROM sys_menu
     WHERE id = v_page
       AND deleted_at IS NULL;

    IF v_path IS DISTINCT FROM '/business/erp/quotation' THEN
        RAISE EXCEPTION 'V0436 自检失败：path 应为 /business/erp/quotation，实际 %', v_path;
    END IF;
    IF v_component IS DISTINCT FROM 'business/erp/quotation/index' THEN
        RAISE EXCEPTION 'V0436 自检失败：component 应为 business/erp/quotation/index，实际 %', v_component;
    END IF;
    IF v_visible IS DISTINCT FROM TRUE THEN
        RAISE EXCEPTION 'V0436 自检失败：报价单页 is_visible 必须为 TRUE（前端页已落地）';
    END IF;
    -- 约定 12：title 是侧栏显示名，2-6 个字、硬顶 12
    IF length(v_title) > 12 THEN
        RAISE EXCEPTION 'V0436 自检失败：报价单页 title 超过 12 字（% 字）', length(v_title);
    END IF;

    -- menu_invariants ④：path 全局唯一（同 path 两行 = 侧栏两条一样的入口）
    SELECT count(*) INTO v_dup
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND menu_type IN (1, 2)
       AND path = '/business/erp/quotation';
    IF v_dup <> 1 THEN
        RAISE EXCEPTION 'V0436 自检失败：path /business/erp/quotation 应恰好 1 行，实际 %', v_dup;
    END IF;

    -- V0433 的 4 个按钮 + V0434 的 convert 必须还挂在页下（本迁移不碰它们，自检防手滑）
    SELECT count(*) INTO v_children
      FROM sys_menu
     WHERE parent_id = v_page
       AND menu_type = 3
       AND deleted_at IS NULL
       AND auth_code IN (
            'business:erp:quotation:create',
            'business:erp:quotation:update',
            'business:erp:quotation:delete',
            'business:erp:quotation:status',
            'business:erp:quotation:convert'
       );
    IF v_children <> 5 THEN
        RAISE EXCEPTION 'V0436 自检失败：报价单页下应有 5 个按钮（V0433 的 4 个 + V0434 的 convert），实际 %', v_children;
    END IF;

    RAISE NOTICE 'V0436：报价单页已可见（path=% component=%），5 个按钮仍在，path 唯一 ✓', v_path, v_component;
END $$;
