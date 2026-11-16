-- ============================================================================
-- V0418__receivable_aging_page（M1-23 · 财务：应收账龄分析页 · 2026-11-16）
--
-- 卡：t_55590fbd（M1-23【M1·财务】账龄：前端页面）
-- 上游：M1-19 应收分档 7 档、M1-20 报表 + 导出、M1-21 应付补分档、M1-22 账龄单测。
--
-- 【本卡做什么】只种**一个页面权限码**（+ 菜单页面行 + 角色授权），不动表结构、不改任何存量行：
--   ① `business:erp:report:receivable-aging`（页面行 menu_type=2，挂「财务管理」…0e02 下，sort_order = 52）
--   ② 授权：SUPER_ADMIN / FINANCE_MANAGER / FINANCE_DIRECTOR / GENERAL_MANAGER
--      —— 与「应付账龄分析」页面行（V0255 …1600）**同一套角色**：两张账龄表是对称的，
--      能看应付账龄的岗位没有理由看不到应收账龄。
--      ⚠ 不放 `TEST_UADMIN`：该码在 `sys_role` 里**不存在**（V0255 时代留下的历史字面量），
--      写上只会静默授 0 行，且 `scripts/verify/role_code_gate.py`（推送闸⑧）会直接判红。
--   ③ 祖先闭包：把父目录「财务管理」（…0e02）补授给上述角色（缺席者才插）。
--   ④ 取数码补齐：把 `business:erp:receivable:aging`（V0288 …2882 按钮行）补授给上面缺它的角色。
--   ⑤ 自检块（节点在位 / 一码一节点 / path 唯一 / title 规范 / 同父 sort 唯一 / 祖先闭包 /
--      「看得到 ⇒ 调得动」），任一不符即 RAISE EXCEPTION 回滚 ——「跑过了」和「验过了」是两件事。
--
-- 【为什么是**新页面**而不是新 tab / 新按钮】
--   这是财务月初的核心动作之一，与「资产负债表 / 利润表 / 现金流量表 / 应付账龄分析 / 外币余额表」
--   同一层级。现有「应收台账」页里的「账龄看板」tab 保留（台账顺手看一眼的场景），
--   但财务不该为了看账龄先点进销售台账 —— 与 V0255 给应付账龄单开一页同口径。
--   现有报表页（V0183 …1015 / V0184 …1017 / V0185 …1018 / V0255 …1600 / V0412 …1750 /
--   V0413 …1760）全部是 menu_type=2 + `path=/business/erp/report/<报表>` +
--   `component` 指向前端页面，本卡照抄这条既有口径（新增报表不发明新结构）。
--
-- 【为什么页面码 ≠ 取数码（本仓最容易被「顺手合并」的一处）】
--   | 码 | 管什么 | 承载行 |
--   |----|--------|--------|
--   | `business:erp:report:receivable-aging` | **页面**：侧栏显形 + 前端 `meta.permission` | 本迁移 …1830 |
--   | `business:erp:receivable:aging`        | **取数**：`GET /business/erp/receivable/aging` | V0288 …2882（已有） |
--   页面码**后端零引用**；取数码**前端零引用**（前端只拿它拼 403 的人话）。
--   两张码的授权必须成对 —— 只授页面码的后果是「菜单点得进去、表格永远 403」，
--   而 403 的报错里只有「无权限」四个字，用户会以为是系统坏了。自检 ⑦ 把这条钉死。
--   （为什么不把页面行的 auth_code 直接写成取数码：那会让同一个码在 `sys_menu` 出现两行
--     —— 角色授权树里两个同名勾选框、勾一个不亮另一个，V0412 的自检注释里已把这种情形判为缺陷。）
--
-- 【祖先闭包为什么必须显式补】
--   `backend/app/src/system/auth/handler.rs::build_menu_tree` 用 `utils::build_tree` 从扁平
--   `parent_id` 建树，**父节点不在授权集合里的子节点会被静默丢弃**（不进 roots、也没人 attach）。
--   实测基线：`财务管理`（…0e02）当前只授给了 EMPLOYEE / FINANCE_MANAGER / GENERAL_MANAGER，
--   而 FINANCE_DIRECTOR 有 `business:erp:report:payable-aging` 页面行却没有父目录 →
--   它的「应付账龄分析」在侧栏里根本不显示。本卡不复刻这个坑：授页面行时一并补父目录。
--
-- 【幂等】`INSERT … ON CONFLICT (id) DO NOTHING` + 授权 `NOT EXISTS` 去重 + 自检块可反复执行；
--   重放不修改任何已存在行的 title / sort_order / 父节点 / path。
--
-- 【回滚】down 先删授权（`sys_role_menu.menu_id` 外键指向 `sys_menu`）再删菜单行；
--   本卡补授的父目录 / 取数码授权也一并回滚（只针对本卡的目标角色，别动历史授权）。
--   回滚 = 应收账龄页下线（接口还在，页面码没人拿得到 ⇒ 除超管外侧栏看不到入口）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：菜单 / 授权 / 取数码承载行必须都在
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0418: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0418: sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = '20000000-0000-0000-0000-000000000e02'::uuid
           AND deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'V0418: 父目录「财务管理」(…0e02) 不存在，先应用 V0154';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = '20000000-0000-0000-0000-000000002882'::uuid
           AND auth_code = 'business:erp:receivable:aging'
           AND deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'V0418: 取数码承载行 business:erp:receivable:aging (…2882) 不存在，先应用 V0288';
    END IF;
    RAISE NOTICE 'V0418: 前置接点齐备（财务管理目录 + 应收账龄取数码），开始种应收账龄页';
END $$;

-- ---------------------------------------------------------------------------
-- ② 权限码 + 菜单页面行
--   parent_id = …0e02「财务管理」；与其余报表页同父。
--   sort_order = 52：同父 menu_type=2 在**全量迁移后**占
--     10/20/21/22/25/27/29/30/40/45/46/47/48/49/50(V0412 外币余额表)/51(V0413 外币明细账)/55，
--     52 空着（约定 12：同父必须唯一）。
--   id = …001830：…0010xx / 0016xx / 0017xx / 0018xx 已分别被应付账龄、报表项、外币小段与
--     现金流底稿（V0415 1770/1780、V0416 1790/1800、V0417 1810/1820）占用；
--     动手前全仓 grep `0000000018` 最大 1800 ⇒ 1830 可用（先落地者定号）。
--   title = 「应收账龄分析」（6 字，≤12；不带说明、不带标点，约定 12）。
--   component = `business/erp/report/receivable-aging/index`
--     （与前端 views 目录逐字一致，路由在 `frontend/src/router/index.ts` 手工注册 —— 两边同名，
--       否则点菜单 404，`system::menu::tests::n08` 会判红）。
--   icon 留 NULL（与 V0255 应付账龄页同口径：报表页不强行配图标，避免引到 TDesign 里不存在的名字）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001830'::uuid, 'business:erp:report:receivable-aging', 2,
     'business:erp:report:receivable-aging', '/business/erp/report/receivable-aging',
     '20000000-0000-0000-0000-000000000e02'::uuid,
     'business/erp/report/receivable-aging/index',
     52, 1, NULL, '应收账龄分析', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ③ 授权：页面行 + 祖先闭包（父目录）
--   目标角色与应付账龄页（V0255）同一套；空库首启时 sys_role 为空 → 空操作，
--   bootstrap 之后重放本迁移即可补齐（自检 ⑧ 会如实报数）。
-- ---------------------------------------------------------------------------
-- ③-1 页面行
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001830'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR',
                       'GENERAL_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm
         WHERE rm.role_id = r.id
           AND rm.menu_id = '20000000-0000-0000-0000-000000001830'::uuid
   );

-- ③-2 祖先闭包：凡**已经**持有本页面行的角色，父目录缺席就补上
--   （父不在授权集合里，子节点会被 build_tree 静默丢掉；这里按「谁有页面行」自愈，
--    管理员日后从界面上给别的角色勾了这一页，重放本迁移也能把父目录补齐）
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), page_rm.role_id, '20000000-0000-0000-0000-000000000e02'::uuid
  FROM sys_role_menu page_rm
  JOIN sys_role r ON r.id = page_rm.role_id AND r.deleted_at IS NULL
 WHERE page_rm.menu_id = '20000000-0000-0000-0000-000000001830'::uuid
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu p
         WHERE p.role_id = page_rm.role_id
           AND p.menu_id = '20000000-0000-0000-0000-000000000e02'::uuid
   );

-- ---------------------------------------------------------------------------
-- ④ 取数码补齐：「看得到 ⇒ 调得动」
--   上面这批角色里 FINANCE_DIRECTOR 原本没有 `business:erp:receivable:aging`
--   （只有 FINANCE_MANAGER / GENERAL_MANAGER / SALES_SUPERVISOR 有）。补的是**同一批角色**，
--   不新增任何岗位：授了页面却调不动接口 = 半成品页面，比不授更糟。
--   SUPER_ADMIN 靠 `*:*:*` 通配本来就能调，这里照插一行只为授权树上看得清范围（V0255 同做法）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000002882'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR',
                       'GENERAL_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm
         WHERE rm.role_id = r.id
           AND rm.menu_id = '20000000-0000-0000-0000-000000002882'::uuid
   );

-- ---------------------------------------------------------------------------
-- ⑤ 迁移自检（任一不符即回滚）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n          integer;
    n_roles    integer;
    n_page     integer;
    n_parent   integer;
    n_data     integer;
    n_closure  integer;
    n_gap      integer;
BEGIN
    -- ===== 1) 节点在位：码 / 类型 / 父节点 / 路径 / 组件 / 启用 / 可见 / 未软删 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001830'::uuid
       AND auth_code = 'business:erp:report:receivable-aging'
       AND name = 'business:erp:report:receivable-aging'
       AND menu_type = 2
       AND parent_id = '20000000-0000-0000-0000-000000000e02'::uuid
       AND path = '/business/erp/report/receivable-aging'
       AND component = 'business/erp/report/receivable-aging/index'
       AND status = 1
       AND is_visible
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0418 自检失败：应收账龄页权限节点不对（页面码授不出去 = 除超管没人能看到入口）';
    END IF;

    -- ===== 2) 一码一节点：同一 auth_code 不许出现在多行（授权页会出现两个同名勾选框）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code = 'business:erp:report:receivable-aging'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0418 自检失败：business:erp:report:receivable-aging 在 sys_menu 里出现 % 次（应为 1）', n;
    END IF;

    -- ===== 3) path 唯一（menu_invariants 第 ④ 条）：同一路径两行 = 侧栏两个入口指向同一页 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE path = '/business/erp/report/receivable-aging'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0418 自检失败：路径 /business/erp/report/receivable-aging 出现 % 次（应为 1）', n;
    END IF;

    -- ===== 4) title 规范：≤12 字、不是功能说明（约定 12 / menu_invariants 第 ⑥ 条）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001830'::uuid
       AND length(title) <= 12
       AND title NOT LIKE '%，%'
       AND title NOT LIKE '%（%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0418 自检失败：应收账龄页 title 不合菜单规范（≤12 字、不带标点/说明）';
    END IF;

    -- ===== 5) 同父 sort_order 唯一（重复会让侧边栏顺序每次刷新都变，约定 12）=====
    SELECT count(*) INTO n
      FROM (
            SELECT sort_order
              FROM sys_menu
             WHERE parent_id = '20000000-0000-0000-0000-000000000e02'::uuid
               AND menu_type = 2
               AND deleted_at IS NULL
             GROUP BY sort_order
            HAVING count(*) > 1
           ) dup;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0418 自检失败：「财务管理」下有 % 组重复的 sort_order（本卡用 52）', n;
    END IF;

    -- ===== 6) 授权计数（有角色就该授到；空库首启时 0/0 且打 NOTICE）=====
    SELECT count(*) INTO n_roles
      FROM sys_role
     WHERE deleted_at IS NULL
       AND role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR',
                         'GENERAL_MANAGER');
    SELECT count(*) INTO n_page
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id = '20000000-0000-0000-0000-000000001830'::uuid
       AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR',
                           'GENERAL_MANAGER');
    IF n_page < n_roles THEN
        RAISE EXCEPTION 'V0418 自检失败：% 个目标角色只授到 % 行页面码', n_roles, n_page;
    END IF;

    -- ===== 7) 祖先闭包：授了页面行就必须授父目录（否则 build_tree 静默丢节点）=====
    SELECT count(*) INTO n_closure
      FROM sys_role_menu page_rm
      JOIN sys_role r ON r.id = page_rm.role_id AND r.deleted_at IS NULL
     WHERE page_rm.menu_id = '20000000-0000-0000-0000-000000001830'::uuid
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu p
             WHERE p.role_id = r.id
               AND p.menu_id = '20000000-0000-0000-0000-000000000e02'::uuid
       );
    IF n_closure <> 0 THEN
        RAISE EXCEPTION
            'V0418 自检失败：% 个角色授了应收账龄页却没授父目录「财务管理」——侧栏里这一项会被静默丢弃',
            n_closure;
    END IF;

    -- ===== 8) 看得到 ⇒ 调得动：页面码之外还必须有取数码（超管靠 *:*:* 通配豁免）
    --   只查本迁移的目标角色集合：别的角色是管理员自己授的，不替人扩权、也不替人判红。
    SELECT count(*) INTO n_gap
      FROM sys_role_menu page_rm
      JOIN sys_role r ON r.id = page_rm.role_id AND r.deleted_at IS NULL
     WHERE page_rm.menu_id = '20000000-0000-0000-0000-000000001830'::uuid
       AND r.role_code IN ('FINANCE_MANAGER', 'FINANCE_DIRECTOR', 'GENERAL_MANAGER')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu d
             WHERE d.role_id = r.id
               AND d.menu_id = '20000000-0000-0000-0000-000000002882'::uuid
       );
    IF n_gap <> 0 THEN
        RAISE EXCEPTION
            'V0418 自检失败：% 个角色有页面码但缺取数码 business:erp:receivable:aging（菜单点得进、表格永远 403）',
            n_gap;
    END IF;

    IF n_roles = 0 THEN
        RAISE NOTICE 'V0418：sys_role 为空（空库首启，迁移先于 bootstrap），授权待重放补齐';
    END IF;

    RAISE NOTICE 'V0418 自检通过：页面码 business:erp:report:receivable-aging（父=财务管理，sort=52）+ 页面授权 %/% 角色 + 父目录与取数码已补齐',
        n_page, n_roles;
END $$;
