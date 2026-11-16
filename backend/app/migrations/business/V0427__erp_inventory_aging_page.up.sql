-- ============================================================================
-- V0427__erp_inventory_aging_page（M2-12 · 仓库：库龄前端页面 · 2026-11-16）
--
-- 卡：t_5e045c0f（M2-12【M2·仓库】库龄：前端页面）
-- 上游：M2-08 库函数 + 批次层接口（V0424 码 …:inventory-aging:list）、
--       M2-09 分段报表（V0425 码 …:inventory-aging:report）、
--       M2-10 呆滞料（V0426 码 …:inventory-aging:slow-moving 与阈值参数）、
--       M2-11 跨期与呆滞判定单测（零迁移）。
--
-- 【本卡做什么】只种**一个页面显形码**（+ 菜单页面行 + 角色授权 + 祖先闭包），
--   不动表结构、不建函数、不碰业务数据、不改任何存量行的 path / title / sort_order：
--     ① `business:erp:inventory-aging:page`（页面行 menu_type=2，挂「仓库管理」…0e01 下，
--        sort_order = 46，path = /business/erp/inventory-aging，
--        component = business/erp/inventory-aging/index）；
--     ② 授权：凡**当前持有**三个取数码中任意一个
--        （`…:inventory-aging:{list,report,slow-moving}`）的角色一并授予页面码 ——
--        「能调库龄接口的人就该看得到这一页」是同一条仓库职责线，数据驱动、不手抄角色名单；
--        超管显式补一行（只为权限树可读，它靠 `*:*:*` 通配本来就能进）。
--     ③ 祖先闭包：给拿到页面码的角色补父目录「仓库管理」（…0e01），缺席者才插。
--     ④ 自检块（节点在位 / 一码一节点 / path 唯一 / title 长度 / 同父 sort 唯一 /
--        祖先闭包 / 「看得到 ⇒ 至少调得动一个 Tab」），任一不符即 RAISE EXCEPTION 回滚。
--
-- 【为什么页面码 ≠ 三个取数码（本卡最容易被「顺手合并」的一处）】
--   | 码 | 管什么 | 承载行 |
--   |----|--------|--------|
--   | `business:erp:inventory-aging:page`   | **页面**：侧栏显形 + 路由 `meta.permission` | 本迁移 …4270 |
--   | `business:erp:inventory-aging:list`   | **取数**：`GET …/inventory-aging/page`（批次层） | V0424 …4240（已有） |
--   | `business:erp:inventory-aging:report` | **取数**：`GET …/inventory-aging/report`（分段报表） | V0425 …4250（已有） |
--   | `business:erp:inventory-aging:slow-moving` | **取数**：`GET …/inventory-aging/slow-moving`（呆滞） | V0426 …4260（已有） |
--
--   ① 同一个码不能有两个承载节点：那会让「角色-菜单」授权页出现两处同名勾选
--      （勾一个另一个不亮），V0412 / V0418 / V0423 的自检注释已把这种情形判为缺陷；
--   ② 更硬的一条：上游三张卡的验证据本把取数码的形状**钉死**了 ——
--      `scripts/verify/m2*_*.sh` 断言各自那个码在 `sys_menu` 里**只有 1 个未删节点且
--      menu_type=3、path IS NULL、父节点是「即时库存」**。本卡把任一取数码挪到页面行都会
--      直接判红上游验收（不是本卡的范围）；
--   ③ 页面码与取数码**成对授予**（见 ③ 与自检 ⑦）：只授页面码的后果是
--      「菜单点得进去、三个 Tab 永远 403」，而 403 原文只有「无权限」四个字。
--   与 `V0418`（应收账龄页）、`V0423`（在途库存页 M2-07）同形 —— 报表页 / 台账页的
--   「显形码」与「取数码」分家是既有口径（AGENTS.md 约定 14 / PERM-01 §6）。
--
-- 【本卡不动 V0424 / V0425 / V0426 的按钮行（含 parent_id）】
--   那三个按钮挂在「即时库存」页面下、且被上游验证据本按父节点名断言过。
--   授权树上它们与「库龄分析」页面行并列看着略乱，但动父节点就会破上游验收 —— 记在
--   docs/evidence/2026-11-16-m212-aging-page/README.md 的「已知形态」里，等 ERP 模块
--   统一解耦（PERM-01 §6）时一并收。
--
-- 【为什么 sort_order = 46】
--   同父（…0e01「仓库管理」）menu_type=2 在**全量迁移后**占
--   5(物料) 10(仓库) 20(库位) 30(序列号) 40(即时库存) 45(在途库存) 50(库存调拨)
--   60(库位库存) 70(库存流水) 80(出库单) 90(盘点单) 95(批次)。
--   46 空着（约定 12：同父 sort 必须唯一），位置正好接在「在途库存」后面 ——
--   两页同属「仓里的货压了多久 / 还在路上多久」这一族。
--
-- 【id 段】4270：4210 是在途查询按钮（V0421）、4220 是对账按钮（V0422）、4230 是在途页面行
--   （V0423）、4240/4250/4260 是库龄三个取数按钮（V0424~V0426）。先落地者定号，本卡取 4270。
--
-- 【title / icon】
--   title = 「库龄分析」（4 字，≤12；侧栏显示名，不带说明、不带标点，约定 12）。
--   icon = 'chart-bar'（TDesign 图标库里真实存在；「分布图」的语义，与即时库存的
--   data-base、在途库存的 swap 区分）。
--   component = `business/erp/inventory-aging/index`（与前端 views 目录逐字一致，
--   路由在 `frontend/src/router/index.ts` 手工注册 —— 漏了 `system::menu::tests::n08` 会判红）。
--
-- 【幂等】`INSERT … ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS` 去重 + 自检块可反复执行；
--   重放不修改任何已存在行的 title / sort_order / 父节点 / path。
--
-- 【回滚】down 先删授权（`sys_role_menu.menu_id` 外键指向 `sys_menu`）再删菜单行；
--   本卡补授的父目录也一并回滚（只针对「因本卡才拿到父目录」的角色）。
--   回滚 = 库龄页下线（三个接口还在，页面码没人拿得到 ⇒ 除超管外侧栏看不到入口）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：菜单 / 授权表在，父目录与三个取数按钮行都在
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_parent integer;
    n_codes  integer;
    n_page   integer;
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0427: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0427: sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;

    -- 父目录「仓库管理」（…0e01，menu_type=1）
    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000e01'::uuid
       AND menu_type = 1
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0427: 父目录「仓库管理」(…0e01) 不存在或不是目录节点，先应用 V0154';
    END IF;

    -- 三个取数按钮行：**每个恰好 1 个** menu_type=3 无 path 的按钮（V0424/V0425/V0426 的形状）
    SELECT count(*) INTO n_codes
      FROM sys_menu
     WHERE auth_code IN ('business:erp:inventory-aging:list',
                         'business:erp:inventory-aging:report',
                         'business:erp:inventory-aging:slow-moving')
       AND menu_type = 3
       AND path IS NULL
       AND deleted_at IS NULL;
    IF n_codes <> 3 THEN
        RAISE EXCEPTION 'V0427: 库龄三个取数码的按钮承载行应有 3 个（menu_type=3 / path IS NULL），实得 % 个 —— 先应用 V0424~V0426', n_codes;
    END IF;

    -- 页面码不能被别人占用（0 或 1：1 = 本迁移重放）
    SELECT count(*) INTO n_page
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-aging:page'
       AND deleted_at IS NULL;
    IF n_page > 1 THEN
        RAISE EXCEPTION 'V0427: business:erp:inventory-aging:page 已有 % 个承载节点（应为 0 或 1）', n_page;
    END IF;

    RAISE NOTICE 'V0427: 前置接点齐备（仓库管理目录 + 库龄三个取数按钮），开始种库龄分析页';
END $$;

-- ---------------------------------------------------------------------------
-- ② 页面显形码 + 菜单页面行
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004270'::uuid, '库龄分析', 2,
     'business:erp:inventory-aging:page', '/business/erp/inventory-aging',
     '20000000-0000-0000-0000-000000000e01'::uuid,
     'business/erp/inventory-aging/index',
     46, 1, 'chart-bar', '库龄分析', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ③ 授权：页面行（跟**任一取数码**的持有角色走 + 超管）
--   按码判重：角色已持有本码则整组跳过（V0258 §4 口径），重复执行零变化。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT rm_src.role_id, m.id
  FROM sys_menu m
  JOIN sys_role_menu rm_src
    ON rm_src.menu_id IN (SELECT id FROM sys_menu
                           WHERE auth_code IN ('business:erp:inventory-aging:list',
                                               'business:erp:inventory-aging:report',
                                               'business:erp:inventory-aging:slow-moving')
                             AND deleted_at IS NULL)
  JOIN sys_role r ON r.id = rm_src.role_id AND r.deleted_at IS NULL
 WHERE m.id = '20000000-0000-0000-0000-000000004270'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = rm_src.role_id AND rm.menu_id = m.id
   );

-- 超管显式补一行（只为权限树可读；它靠 *:*:* 通配本来就能进）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
 CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND m.id = '20000000-0000-0000-0000-000000004270'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ④ 祖先闭包：「仓库管理」目录（…0e01）
--   `system/auth/handler.rs::build_menu_tree` 用 `utils::build_tree` 从扁平 parent_id 建树，
--   **父节点不在授权集合里的子节点会被静默丢弃** ⇒ 角色有页面行却没有父目录时，
--   侧栏里整棵子树消失。这里按「谁有页面行」自愈（可重放）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT page_rm.role_id, '20000000-0000-0000-0000-000000000e01'::uuid
  FROM sys_role_menu page_rm
  JOIN sys_role r ON r.id = page_rm.role_id AND r.deleted_at IS NULL
 WHERE page_rm.menu_id = '20000000-0000-0000-0000-000000004270'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu p
         WHERE p.role_id = page_rm.role_id
           AND p.menu_id = '20000000-0000-0000-0000-000000000e01'::uuid
   );

-- ---------------------------------------------------------------------------
-- ⑤ 迁移自检（任一不符即回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n          integer;
    n_roles    integer;
    n_page     integer;
    n_same     integer;
    n_path     integer;
    n_sort     integer;
    n_gap      integer;   -- 「看得到却一个 Tab 都调不动」的角色数
    n_orphan   integer;   -- 有页面行却没有祖先目录的角色数
    v          text;
BEGIN
    -- ===== 1) 节点在位：码 / 类型 / 父节点 / 路径 / 组件 / 启用 / 可见 / 未软删 / title / icon =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004270'::uuid
       AND auth_code = 'business:erp:inventory-aging:page'
       AND name = '库龄分析'
       AND menu_type = 2
       AND parent_id = '20000000-0000-0000-0000-000000000e01'::uuid
       AND path = '/business/erp/inventory-aging'
       AND component = 'business/erp/inventory-aging/index'
       AND sort_order = 46
       AND status = 1
       AND is_visible
       AND title = '库龄分析'
       AND icon = 'chart-bar'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0427 自检失败：库龄分析页节点不对（页面码授不出去 = 除超管没人能看到入口）';
    END IF;

    -- ===== 2) 一码一节点（同码两行 = 授权页两个同名勾选框）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code = 'business:erp:inventory-aging:page'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0427 自检失败：business:erp:inventory-aging:page 在 sys_menu 里出现 % 次（应为 1）', n;
    END IF;

    -- ===== 3) 三个取数码仍是「各 1 个按钮节点」—— 本迁移不得改变上游形状 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code IN ('business:erp:inventory-aging:list',
                         'business:erp:inventory-aging:report',
                         'business:erp:inventory-aging:slow-moving')
       AND menu_type = 3
       AND deleted_at IS NULL;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0427 自检失败：库龄三个取数码的按钮节点数变成 %（上游 M2-08~M2-10 验证据本要求各恰好 1 个且 menu_type=3）', n;
    END IF;

    -- ===== 4) path 唯一（menu_invariants 第 ④ 条）=====
    SELECT count(*) INTO n_path
      FROM sys_menu
     WHERE path = '/business/erp/inventory-aging'
       AND deleted_at IS NULL;
    IF n_path <> 1 THEN
        RAISE EXCEPTION 'V0427 自检失败：path /business/erp/inventory-aging 出现 % 行（应为 1，侧栏会出现两个入口）', n_path;
    END IF;

    -- ===== 5) 同父 menu_type=2 的 sort_order 唯一（约定 12）=====
    SELECT count(*) INTO n_sort
      FROM (
            SELECT parent_id, sort_order
              FROM sys_menu
             WHERE parent_id = '20000000-0000-0000-0000-000000000e01'::uuid
               AND menu_type = 2
               AND deleted_at IS NULL
             GROUP BY parent_id, sort_order
            HAVING count(*) > 1
           ) dup;
    IF n_sort <> 0 THEN
        RAISE EXCEPTION 'V0427 自检失败：仓库管理目录下有 % 组重复 sort_order（约定 12）', n_sort;
    END IF;

    -- ===== 6) title 长度 ≤ 12（menu_invariants 第 ⑥ 条；侧栏显示名不是功能说明）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004270'::uuid
       AND length(title) <= 12;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0427 自检失败：页面 title 超过 12 字（侧栏会换行/截断）';
    END IF;

    -- ===== 7) 授权：角色存在时，「有页面行 ⇒ 至少有一个取数码」与「有页面行 ⇒ 有祖先目录」都要成立 =====
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0427：库里还没有任何角色（迁移先于种子），跳过授权核对；'
                     '种子/bootstrap 之后重放本迁移即可补齐（幂等）';
    ELSE
        -- 7.1 看得到 ⇒ 至少调得动一个 Tab：三个取数码一个都没有的角色必须为空集合
        SELECT count(*), string_agg(r.role_code, ', ') INTO n_gap, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id = '20000000-0000-0000-0000-000000004270'::uuid)
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                             JOIN sys_menu m ON m.id = rm.menu_id
                            WHERE rm.role_id = r.id
                              AND m.auth_code IN ('business:erp:inventory-aging:list',
                                                  'business:erp:inventory-aging:report',
                                                  'business:erp:inventory-aging:slow-moving')
                              AND m.deleted_at IS NULL);
        IF n_gap <> 0 THEN
            RAISE EXCEPTION 'V0427 自检失败：% 个角色拿到了页面码却没有任何取数码（菜单点得进、三个 Tab 全 403）：%',
                n_gap, COALESCE(v, '');
        END IF;

        -- 7.2 祖先闭包：有页面行却没有「仓库管理」目录的角色必须为空集合（否则侧栏静默消失）
        SELECT count(*), string_agg(r.role_code, ', ') INTO n_orphan, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id = '20000000-0000-0000-0000-000000004270'::uuid)
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id = '20000000-0000-0000-0000-000000000e01'::uuid);
        IF n_orphan <> 0 THEN
            RAISE EXCEPTION 'V0427 自检失败：% 个角色有页面行却没有祖先目录「仓库管理」（侧栏整棵子树会消失）：%',
                n_orphan, COALESCE(v, '');
        END IF;

        SELECT count(DISTINCT role_id) INTO n_page
          FROM sys_role_menu
         WHERE menu_id = '20000000-0000-0000-0000-000000004270'::uuid;
        RAISE NOTICE 'V0427 自检通过：页面授权 %/% 角色（含超管），取数码与祖先目录均已成对', n_page, n_roles;
    END IF;
END $$;
