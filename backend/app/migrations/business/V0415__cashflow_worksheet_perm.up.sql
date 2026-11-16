-- ============================================================================
-- V0415__cashflow_worksheet_perm（M1-14 · 现金流底稿录入 + 勾稽校验的权限码 · 2026-11-16）
--
-- 卡：t_9e14a327（M1-14【M1·财务】现金流底稿：录入 + 勾稽校验（直接法））
-- 上游：t_31c91cd5（M1-13）的 V0414 已建表 `biz_cashflow_worksheet`；本迁移**不动表结构**。
--
-- 【本卡做什么】只种两个**按钮**权限码（menu_type=3），挂在既有「现金流量表」页面
-- （`…001018`，V0185 种）下，并授权给财务相关角色：
--
--   | 码 | 承载接口 | 为什么单独授权 |
--   |----|----------|----------------|
--   | `business:erp:cashflow-worksheet:list` | GET 底稿、POST 校验（只读） | 看报表依据：财务经理 / 总监 / 审计都要看 |
--   | `business:erp:cashflow-worksheet:save` | PUT 录入（写） | **改**报表依据：改错一分钱，出的现金流量表就错一分钱且看不出来 |
--
-- 【为什么是按钮行（menu_type=3）而不是新开一个页面（menu_type=2）】
--   底稿不是一张独立报表，而是「现金流量表这一页的编制依据」——它的产出物就是那张表。
--   前端（M1-18）把底稿做成现金流量表页面里的一个视图 / 抽屉，路由与页面权限仍归
--   `/business/erp/report/cash-flow`（V0185 的 `business:erp:report:cash-flow`）。
--   种一个 `menu_type=2` 页面行会让侧边栏凭空多一项指向尚未存在的视图（点进去 404），
--   而按钮行没有 path，只出现在角色授权树里 —— 这才是「接口码」该待的位置。
--   与 HR 的页面码 / 接口码解耦（编码规范 §1.7.2）方向一致：ERP 报表侧尚未解耦，
--   本卡不夹带改造，只把自己的两个接口码挂在承载它的页面下。
--
-- 【为什么校验接口复用读码】
--   `POST …/worksheet/check` 一个字节都不写库（纯计算：把草稿叠加到已存底稿上算勾稽）。
--   能看底稿的人当然能校验底稿；另开一个码只会逼管理员为了「点一下校验」再授一次。
--
-- 【幂等】
--   菜单 `ON CONFLICT (id) DO NOTHING`；授权 `NOT EXISTS` 去重；自检块可反复执行。
--   重放本迁移不会改动任何已存在行的 title / sort_order / 父节点（只补缺失行）。
--
-- 【回滚】
--   down 先删授权（`sys_role_menu` 有外键指向 `sys_menu`）再删两行菜单；
--   **加护栏**：底稿表里已有行（有人真的编过底稿）时拒绝回滚 —— 回滚权限码 = 功能下线，
--   底稿还在但谁都改不了，只能由工程师改库。要真回滚请先由 DBA 确认底稿已导出 / 作废。
--   （与 V0414 的 down 护栏同一口径。）
--
-- 【id 段】…0017xx 是 V0408 起的「外币 / 报表」小段（1720 科目外币核算、1730 期末调汇、
--   1740 汇兑损益科目、1750 外币余额表、1760 外币总账）；本卡续 1770 / 1780。
--   卡面 §「先落地者定号」：动手前已 `grep -r '20000000-0000-0000-0000-0000000017'`
--   核对，最大已用 1760。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：菜单 / 授权 / 承载体 / 底稿表必须都在
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0415: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0415: sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.biz_cashflow_worksheet') IS NULL THEN
        RAISE EXCEPTION 'V0415: biz_cashflow_worksheet 不存在，请先应用 V0414（M1-13 现金流底稿表结构）';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = '20000000-0000-0000-0000-000000001018'::uuid
           AND deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'V0415: 「现金流量表」页面节点 …001018 不存在（V0185 没跑到），按钮行无处可挂';
    END IF;
    RAISE NOTICE 'V0415: 前置表齐备，开始种现金流底稿权限码';
END $$;

-- ---------------------------------------------------------------------------
-- ② 两个按钮权限码（同父 = 「现金流量表」页面 …001018）
--   sort_order 10 / 20：该页面此前没有任何按钮行（V0185 只种了页面行），从 10 起。
--   title 按约定 12：≤12 字、不带标点与说明（「底稿查看」「底稿录入」各 4 字）。
--   path = NULL：按钮行没有路由，只承载 auth_code（与全仓其余 button 行同形）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001770'::uuid,
     'business:erp:cashflow-worksheet:list', 3,
     'business:erp:cashflow-worksheet:list', NULL,
     '20000000-0000-0000-0000-000000001018'::uuid, NULL,
     10, 1, NULL, '底稿查看', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001780'::uuid,
     'business:erp:cashflow-worksheet:save', 3,
     'business:erp:cashflow-worksheet:save', NULL,
     '20000000-0000-0000-0000-000000001018'::uuid, NULL,
     20, 1, NULL, '底稿录入', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：超管 / 财务经理
--   · SUPER_ADMIN：空库首启时 sys_role 为空 → 本节空操作（超管靠 `*:*:*` 通配照样能进）；
--     有角色后重放本迁移补齐，角色树上能看清范围（与 V0410 / V0411 / V0412 同口径）。
--   · FINANCE_MANAGER：编底稿的主力（V0185 已给它现金流量表页面码 `business:erp:report:cash-flow`，
--     按钮挂在该页面下 ⇒ 可达）。
--   **刻意不给 FINANCE_DIRECTOR**：它没有现金流量表页面码，给了按钮就是「孤儿按钮」
--   （`orphan_button_gate.py` 判红：不可达父页），只能靠直连 API 用到，界面上点不进去。
--   要放开得先给它报表页码 —— 那是另一张卡的事，本卡不夹带。
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001770'::uuid),
        ('20000000-0000-0000-0000-000000001780'::uuid)
  ) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER')
   AND NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm
         WHERE rm.role_id = r.id
           AND rm.menu_id = m.id
   );

-- ---------------------------------------------------------------------------
-- ③ 迁移自检（节点 / 一码一节点 / 同父 sort 唯一 / title 规范 / 授权到位）
--   任一不符即 RAISE EXCEPTION 让整个迁移回滚 ——「跑过了」和「验过了」是两件事。
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n        integer;
    n_roles  integer;
    n_grant  integer;
BEGIN
    -- ===== 两个节点都在位：码 / 名称 / 类型 / 父节点 / 无 path / 启用 / 未软删 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001770'::uuid,
                  '20000000-0000-0000-0000-000000001780'::uuid)
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000001018'::uuid
       AND path IS NULL
       AND status = 1
       AND deleted_at IS NULL
       AND auth_code = name;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0415 自检失败：两个底稿按钮节点只建成 % 个（码授不出去 = 除超管没人能编底稿）', n;
    END IF;

    -- ===== 一码一节点：同一个 auth_code 不许出现在多行（重复行会让授权页出现两个同名勾选框）=====
    SELECT count(*) INTO n
      FROM (
            SELECT auth_code
              FROM sys_menu
             WHERE auth_code IN ('business:erp:cashflow-worksheet:list',
                                 'business:erp:cashflow-worksheet:save')
               AND deleted_at IS NULL
             GROUP BY auth_code
            HAVING count(*) = 1
           ) one_node;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0415 自检失败：两个码在 sys_menu 里应各恰好 1 行（实际只 % 个码满足）', n;
    END IF;

    -- ===== title 规范：≤12 字、不带标点/说明（menu_invariants 第 6 条）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001770'::uuid,
                  '20000000-0000-0000-0000-000000001780'::uuid)
       AND length(title) <= 12
       AND title NOT LIKE '%，%'
       AND title NOT LIKE '%（%'
       AND btrim(title) <> '';
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0415 自检失败：底稿按钮节点的 title 不合菜单规范（≤12 字、不带标点/说明）';
    END IF;

    -- ===== 同父 sort_order 唯一（重复会让授权树顺序每次刷新都变，约定 12）=====
    SELECT count(*) INTO n
      FROM (
            SELECT sort_order
              FROM sys_menu
             WHERE parent_id = '20000000-0000-0000-0000-000000001018'::uuid
               AND menu_type = 3
               AND deleted_at IS NULL
             GROUP BY sort_order
            HAVING count(*) > 1
           ) dup;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0415 自检失败：「现金流量表」页面下有 % 组重复的 sort_order（本卡用 10 / 20）', n;
    END IF;

    -- ===== 授权：有角色就该授到；角色不存在（空库首启）则跳过 =====
    SELECT count(*) INTO n_roles
      FROM sys_role
     WHERE deleted_at IS NULL
       AND role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    SELECT count(*) INTO n_grant
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id IN ('20000000-0000-0000-0000-000000001770'::uuid,
                          '20000000-0000-0000-0000-000000001780'::uuid)
       AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    IF n_grant <> n_roles * 2 THEN
        RAISE EXCEPTION 'V0415 自检失败：授权数 % ≠ 目标角色数 % × 2 个码（有角色没授到 → 有人点不开底稿）',
            n_grant, n_roles;
    END IF;

    RAISE NOTICE 'V0415 自检通过：现金流底稿 2 个权限码（底稿查看 / 底稿录入）挂「现金流量表」页面，授权 % 个角色 × 2 码 = % 行',
        n_roles, n_grant;
END $$;
