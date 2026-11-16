-- ============================================================================
-- V0431__erp_stock_policy_page（M2-17 · 仓库：安全库存设置页 · 2026-11-17）
--
-- 卡：t_58a23185（M2-17【M2·仓库】安全库存：前端设置页 —— 批量设置 + 导入）
-- 上游：M2-13 表 + 设置/列表/删除接口 + 三个码（V0428）、
--       M2-14 水位检查 + 检查码（V0429）、M2-15 补货建议（V0430 及之后的代码）、
--       M2-16 单测（零迁移）。
--
-- 【本卡做什么】只种**一个页面显形码**（+ 菜单页面行 + 角色授权 + 祖先闭包），
--   不动表结构、不建函数、不碰业务数据、不改任何存量行的 path / title / sort_order：
--     ① `business:erp:stock-policy:page`（页面行 menu_type=2，挂「仓库管理」…0e01 下，
--        sort_order = 47，path = /business/erp/stock-policy，
--        component = business/erp/stock-policy/index）；
--     ② 授权：凡**当前持有**四个水位码中任意一个
--        （`…:stock-policy:{list,save,delete,check}`）的角色一并授予页面码 ——
--        「能设水位 / 能跑水位检查的人就该看得到这一页」是同一条仓库职责线，
--        数据驱动、不手抄角色名单；超管显式补一行（只为权限树可读，它靠 `*:*:*` 本来就能进）；
--     ③ 祖先闭包：给拿到页面码的角色补父目录「仓库管理」（…0e01），缺席者才插；
--     ④ 自检块（节点在位 / 一码一节点 / path 唯一 / title 长度 / 同父 sort 唯一 /
--        祖先闭包 / 「看得到就至少调得动一个接口」），任一不符即 RAISE EXCEPTION 回滚。
--
-- 【为什么页面码 ≠ 四个水位码（本卡最容易被「顺手合并」的一处）】
--   | 码 | 管什么 | 承载行 |
--   |----|--------|--------|
--   | `business:erp:stock-policy:page`    | **页面**：侧栏显形 + 路由 `meta.permission` | 本迁移 …4310 |
--   | `business:erp:stock-policy:list`    | **取数**：`GET …/stock-policy/page`      | V0428 …4280（已有） |
--   | `business:erp:stock-policy:save`    | **写入**：`PUT …/stock-policy` + `PUT …/stock-policy/batch` | V0428 …4281（已有） |
--   | `business:erp:stock-policy:delete`  | **写入**：`DELETE …/stock-policy/{id}`   | V0428 …4282（已有） |
--   | `business:erp:stock-policy:check`   | **动作**：`POST …/stock-policy/check`    | V0429 …4290（已有） |
--
--   ① 同一个码不能有两个承载节点：那会让「角色-菜单」授权页出现两处同名勾选（勾一个另一个不亮）；
--   ② 更硬的一条：V0428 / V0429 的自检把四个码的形状钉死成「menu_type=3 / path IS NULL /
--      挂在「即时库存」页下」，本卡把任一码挪到页面行都会直接破它们；
--   ③ 页面码与接口码**成对授予**（见 ③ 与自检 ⑧）：只授页面码的后果是
--      「菜单点得进去、列表永远 403」，而 403 原文只有「无权限」四个字。
--   与 V0423（在途库存页）、V0427（库龄分析页）同形 —— 页面显形码与接口码分家是既有口径
--   （AGENTS.md 约定 14 / PERM-01 §6）。
--
-- 【为什么 sort_order = 47】
--   同父（…0e01「仓库管理」）menu_type=2 在全量迁移后占
--   5(物料) 10(仓库) 20(库位) 30(序列号) 40(即时库存) 45(在途库存) 46(库龄分析)
--   50(库存调拨) 60(库位库存) 70(库存流水) 80(出库单) 90(盘点单) 95(批次)。
--   47 空着（约定 12：同父 sort 必须唯一），位置紧接「库龄分析」——
--   三页同属「仓里的货还剩多少 / 压了多久 / 该补多少」这一族。
--
-- 【id 段】4310：4210/4220 在途按钮、4230 在途页面行、4240~4260 库龄三个取数按钮、
--   4270 库龄页面行、4280~4282 水位三码、4290 水位检查码、4300 段属 M2-15 补货建议。
--   先落地者定号，本卡取 4310。
--
-- 【title / icon】
--   title = 「安全库存」（4 字，≤12；侧栏显示名，不带说明、不带标点，约定 12）。
--   icon = 'setting'（TDesign 图标库里真实存在；「水位设置」的语义，与库龄的 chart-bar、
--   在途的 swap 区分）。名字必须逐字在 `frontend/src/components/IconSelect/icons.ts` 里 ——
--   写错不报错，侧栏那一项就是一片空白（menu_invariants 第 ⑧ 条会判红）。
--   component = `business/erp/stock-policy/index`（与前端 views 目录逐字一致，
--   路由在 `frontend/src/router/index.ts` 手工注册 —— 漏了 `system::menu::tests::n08` 会判红）。
--
-- 【幂等】`INSERT … ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS` 去重 + 自检块可反复执行；
--   重放不修改任何已存在行的 title / sort_order / 父节点 / path。
--
-- 【回滚】down 先删授权（`sys_role_menu.menu_id` 外键指向 `sys_menu`）再删菜单行；
--   本卡补授的父目录也一并回滚（只针对「因本卡才拿到父目录」的角色）。
--   回滚 = 设置页下线（四个接口还在，页面码没人拿得到 ⇒ 除超管外侧栏看不到入口）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：菜单 / 授权表在，父目录与四个水位码都在，页面码没被别人占
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_parent integer;
    n_codes  integer;
    n_page   integer;
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0431: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0431: sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;

    -- 父目录「仓库管理」（…0e01，menu_type=1）
    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000e01'::uuid
       AND menu_type = 1
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0431: 父目录「仓库管理」(…0e01) 不存在或不是目录节点，先应用 V0154';
    END IF;

    -- 四个水位码：每个恰好 1 个 menu_type=3 无 path 的按钮（V0428 / V0429 的形状）
    SELECT count(*) INTO n_codes
      FROM sys_menu
     WHERE auth_code IN ('business:erp:stock-policy:list',
                         'business:erp:stock-policy:save',
                         'business:erp:stock-policy:delete',
                         'business:erp:stock-policy:check')
       AND menu_type = 3
       AND path IS NULL
       AND deleted_at IS NULL;
    IF n_codes <> 4 THEN
        RAISE EXCEPTION 'V0431: 水位四个码的按钮承载行应有 4 个（menu_type=3 / path IS NULL），实得 % 个 —— 先应用 V0428 与 V0429', n_codes;
    END IF;

    -- 页面码不能被别人占用（0 或 1：1 = 本迁移重放）
    SELECT count(*) INTO n_page
      FROM sys_menu
     WHERE auth_code = 'business:erp:stock-policy:page'
       AND deleted_at IS NULL;
    IF n_page > 1 THEN
        RAISE EXCEPTION 'V0431: business:erp:stock-policy:page 已有 % 个承载节点（应为 0 或 1）', n_page;
    END IF;

    RAISE NOTICE 'V0431: 前置接点齐备（仓库管理目录 + 水位四个码），开始种安全库存页';
END $$;

-- ---------------------------------------------------------------------------
-- ② 页面显形码 + 菜单页面行
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004310'::uuid, '安全库存', 2,
     'business:erp:stock-policy:page', '/business/erp/stock-policy',
     '20000000-0000-0000-0000-000000000e01'::uuid,
     'business/erp/stock-policy/index',
     47, 1, 'setting', '安全库存', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ③ 授权：页面行（跟**任一个水位码**的持有角色走 + 超管）
--   按码判重：角色已持有本码则整组跳过，重复执行零变化。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT rm_src.role_id, m.id
  FROM sys_menu m
  JOIN sys_role_menu rm_src
    ON rm_src.menu_id IN (SELECT id FROM sys_menu
                           WHERE auth_code IN ('business:erp:stock-policy:list',
                                               'business:erp:stock-policy:save',
                                               'business:erp:stock-policy:delete',
                                               'business:erp:stock-policy:check')
                             AND deleted_at IS NULL)
  JOIN sys_role r ON r.id = rm_src.role_id AND r.deleted_at IS NULL
 WHERE m.id = '20000000-0000-0000-0000-000000004310'::uuid
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
   AND m.id = '20000000-0000-0000-0000-000000004310'::uuid
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
 WHERE page_rm.menu_id = '20000000-0000-0000-0000-000000004310'::uuid
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
    n_same     integer;
    n_path     integer;
    n_sort     integer;
    n_gap      integer;   -- 「看得到却一个接口都调不动」的角色数
    n_orphan   integer;   -- 有页面行却没有祖先目录的角色数
    n_codes    integer;
    v          text;
BEGIN
    -- ===== 1) 节点在位：码 / 类型 / 父节点 / 路径 / 组件 / 启用 / 可见 / 未软删 / title / icon =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004310'::uuid
       AND auth_code = 'business:erp:stock-policy:page'
       AND name = '安全库存'
       AND menu_type = 2
       AND parent_id = '20000000-0000-0000-0000-000000000e01'::uuid
       AND path = '/business/erp/stock-policy'
       AND component = 'business/erp/stock-policy/index'
       AND sort_order = 47
       AND status = 1
       AND is_visible
       AND title = '安全库存'
       AND icon = 'setting'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0431 自检失败：安全库存页节点不对（页面码授不出去 = 除超管没人能看到入口）';
    END IF;

    -- ===== 2) 一码一节点（同码两行 = 授权页两个同名勾选框）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code = 'business:erp:stock-policy:page'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0431 自检失败：business:erp:stock-policy:page 在 sys_menu 里出现 % 次（应为 1）', n;
    END IF;

    -- ===== 3) 四个水位码仍是「各 1 个按钮节点」—— 本迁移不得改变上游形状 =====
    SELECT count(*) INTO n_codes
      FROM sys_menu
     WHERE auth_code IN ('business:erp:stock-policy:list',
                         'business:erp:stock-policy:save',
                         'business:erp:stock-policy:delete',
                         'business:erp:stock-policy:check')
       AND menu_type = 3
       AND deleted_at IS NULL;
    IF n_codes <> 4 THEN
        RAISE EXCEPTION 'V0431 自检失败：水位四个码的按钮节点数变成 %（V0428/V0429 要求各恰好 1 个且 menu_type=3）', n_codes;
    END IF;

    -- ===== 4) path 唯一（menu_invariants 第 ④ 条）=====
    SELECT count(*) INTO n_path
      FROM sys_menu
     WHERE path = '/business/erp/stock-policy'
       AND deleted_at IS NULL;
    IF n_path <> 1 THEN
        RAISE EXCEPTION 'V0431 自检失败：path /business/erp/stock-policy 出现 % 行（应为 1，侧栏会出现两个入口）', n_path;
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
        RAISE EXCEPTION 'V0431 自检失败：仓库管理目录下有 % 组重复 sort_order（约定 12）', n_sort;
    END IF;

    -- ===== 6) title 长度 ≤12、无标点（约定 12 / menu_invariants 第 ⑥ 条）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004310'::uuid
       AND (length(title) > 12 OR title ~ '[，。：；、（）()【】]');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0431 自检失败：安全库存页 title 超过 12 字或带标点（约定 12）';
    END IF;

    -- ===== 7) 授权完整性 =====
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0431：库里还没有任何角色（迁移先于种子），跳过授权核对；种子/bootstrap 之后重放本迁移即可补齐（幂等）';
    ELSE
        -- 7.1 「看得到却调不动」：拿到页面码但四个接口码一个都没有（点进去只会 403）
        SELECT count(*), string_agg(DISTINCT r.role_code, ', ') INTO n_gap, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id = '20000000-0000-0000-0000-000000004310'::uuid)
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm
                  JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
                 WHERE rm.role_id = r.id
                   AND m.auth_code IN ('business:erp:stock-policy:list',
                                       'business:erp:stock-policy:save',
                                       'business:erp:stock-policy:delete',
                                       'business:erp:stock-policy:check')
               );
        IF n_gap <> 0 THEN
            RAISE EXCEPTION 'V0431 自检失败：% 个角色有页面码却没有任何水位接口码（进页必 403）→ %', n_gap, COALESCE(v, '—');
        END IF;

        -- 7.2 祖先闭包：有页面行的角色必须有「仓库管理」目录
        SELECT count(*), string_agg(DISTINCT r.role_code, ', ') INTO n_orphan, v
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = r.id
                          AND rm.menu_id = '20000000-0000-0000-0000-000000004310'::uuid)
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                            WHERE rm.role_id = r.id
                              AND rm.menu_id = '20000000-0000-0000-0000-000000000e01'::uuid);
        IF n_orphan <> 0 THEN
            RAISE EXCEPTION 'V0431 自检失败：% 个角色有安全库存页面行却没有「仓库管理」目录（侧栏整棵子树会消失）→ %', n_orphan, COALESCE(v, '—');
        END IF;

        IF NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_role r ON r.id = rm.role_id
             WHERE r.role_code = 'SUPER_ADMIN'
               AND rm.menu_id = '20000000-0000-0000-0000-000000004310'::uuid
        ) THEN
            RAISE NOTICE 'V0431：超管尚无显式授权行（空库首启时超管由种子晚于本迁移创建；重放本迁移即可补齐）';
        END IF;
    END IF;

    -- ===== 8) 一码一节点（全模块 5 个码）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code LIKE 'business:erp:stock-policy:%'
       AND deleted_at IS NULL;
    IF n <> 5 THEN
        RAISE EXCEPTION 'V0431 自检失败：stock-policy 系列码共 % 个未删节点（应为 5 = 1 页面 + 4 按钮）', n;
    END IF;

    SELECT count(DISTINCT role_id) INTO n_roles
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000004310'::uuid;
    RAISE NOTICE 'V0431: 自检通过（页面行挂「仓库管理」sort 47 / 五码各一节点 / 授权 % 个角色 / 祖先闭包齐）',
        n_roles;
END $$;
