-- ============================================================================
-- V0417__cashflow_statement_perm（M1-16 · 由底稿生成现金流量表的权限码 · 2026-11-16）
--
-- 卡：t_74e707ca（M1-16【M1·财务】现金流底稿：由底稿生成现金流量表）
-- 上游：V0414（M1-13 底稿表）、V0415（M1-14 底稿录入两码）、V0416（M1-15 调整项两码）。
-- 本迁移**不动表结构、不改任何存量行**，只种两个按钮权限码。
--
-- 【本卡做什么】给「由底稿出表」的两个接口各配一个码，挂在既有「现金流量表」页面
-- （`…001018`，V0185 种）下：
--
--   | 码 | 承载接口 | 为什么单独授权 |
--   |----|----------|----------------|
--   | `business:erp:cashflow-statement:list`   | GET `…/cash-flow/statement`         | 看**出表结果**：出表就是对外报出的那张纸，能看的人与能看底稿的人不必完全相同（总监 / 审计要看表，不一定要看编制过程） |
--   | `business:erp:cashflow-statement:export` | GET `…/cash-flow/statement/export`  | **导出 / 打印**：一旦落成 xlsx 就会流出系统，与「在页面里看」是两种风险等级 |
--
-- 【为什么不复用 V0415 的 `cashflow-worksheet:list` / `:save`】
--   底稿与报表是两件事，两个风险面：
--     · 底稿 = 编制过程（逐行金额、备注、期初期末余额都暴露，且可被改写）；
--     · 报表 = 编制结果（三栏汇总，只读）。
--   把出表塞进底稿的读码，等于「能看底稿的人必然能导出报表」，而导出件会离开系统
--   （邮件 / 打印 / 报送给银行与税务）。反过来也不成立：FINANCE_DIRECTOR 这类
--   只审结果的岗位，授出表码但不授底稿码才合理。所以必须是独立的码。
--   与 ERP 报表侧既有分工同构：`business:erp:report:cash-flow`（看表）与
--   `business:erp:report:export`（导出）本来就是两个码（V0185 / V0186）。
--
-- 【为什么是按钮行（menu_type=3）而不是新开页面（menu_type=2）】
--   出表结果不是一张独立报表页，而是「现金流量表」这一页里从底稿切过去的视图
--   （前端 M1-18 把底稿 / 报表做成同页两个 tab）。种 `menu_type=2` 会让侧边栏凭空
--   多一项指向不存在的视图（点进去 404）。按钮行没有 path，只出现在角色授权树里 ——
--   这正是「接口码」该待的位置（与 V0415 / V0416 同口径）。
--
-- 【幂等】
--   菜单 `ON CONFLICT (id) DO NOTHING`；授权 `NOT EXISTS` 去重；自检块可反复执行。
--   重放不修改任何已存在行的 title / sort_order / 父节点，只补缺失行。
--
-- 【回滚】
--   down 先删授权（`sys_role_menu.menu_id` 有外键指向 `sys_menu`）再删两行菜单。
--   回滚 = **出表功能下线**（接口还在，两个码谁都拿不到 ⇒ 除超管外没人能调）。
--   本迁移不新增任何业务数据，所以没有数据护栏；真要回滚请先确认没有人依赖已导出的表。
--
-- 【id 段】…0017xx 是 V0408 起的「外币 / 报表」小段；本卡续 1810 / 1820。
--   卡面 §「先落地者定号」：动手前已核对 V0415 用 1770/1780、V0416 用 1790/1800，
--   全仓 `grep '20000000-0000-0000-0000-0000000018'` 最大 1800 ⇒ 1810 / 1820 可用。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：菜单 / 授权 / 承载页面必须都在
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0417: sys_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF to_regclass('public.sys_role_menu') IS NULL THEN
        RAISE EXCEPTION 'V0417: sys_role_menu 不存在，请先应用系统底座迁移';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM sys_menu
         WHERE id = '20000000-0000-0000-0000-000000001018'::uuid
           AND deleted_at IS NULL
    ) THEN
        RAISE EXCEPTION 'V0417: 「现金流量表」页面节点 …001018 不存在（V0185 没跑到），按钮行无处可挂';
    END IF;
    RAISE NOTICE 'V0417: 前置表齐备，开始种现金流量表出表权限码';
END $$;

-- ---------------------------------------------------------------------------
-- ② 两个按钮权限码（同父 = 「现金流量表」页面 …001018；sort 50 / 60 接在
--   底稿 10/20（V0415）与调整项 30/40（V0416）之后 —— 同父 sort 必须唯一，约定 12）
--   title 按约定 12：≤12 字、不带标点与说明（「出表查看」「出表导出」各 4 字）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001810'::uuid,
     'business:erp:cashflow-statement:list', 3,
     'business:erp:cashflow-statement:list', NULL,
     '20000000-0000-0000-0000-000000001018'::uuid, NULL,
     50, 1, NULL, '出表查看', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000001820'::uuid,
     'business:erp:cashflow-statement:export', 3,
     'business:erp:cashflow-statement:export', NULL,
     '20000000-0000-0000-0000-000000001018'::uuid, NULL,
     60, 1, NULL, '出表导出', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权：超管 / 财务经理
--   · SUPER_ADMIN：空库首启时 sys_role 为空 → 本节空操作（超管靠 `*:*:*` 通配照样能进）；
--     有角色后重放本迁移补齐，角色树上能看清范围（与 V0410 / V0411 / V0415 / V0416 同口径）。
--   · FINANCE_MANAGER：编底稿并按期出表的主力（已有该页面码 `business:erp:report:cash-flow`
--     与底稿两码，按钮挂在该页面下 ⇒ 可达）。
--   **刻意不给 FINANCE_DIRECTOR**：它没有现金流量表页面码，给了按钮就是「孤儿按钮」
--   （`orphan_button_gate.py` 判红：不可达父页）。要放开得先给它报表页码 —— 另一张卡的事。
INSERT INTO sys_role_menu (id, role_id, menu_id)
SELECT gen_random_uuid(), r.id, m.id
  FROM sys_role r
  CROSS JOIN (VALUES
        ('20000000-0000-0000-0000-000000001810'::uuid),
        ('20000000-0000-0000-0000-000000001820'::uuid)
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
--   任一不符即 RAISE EXCEPTION 回滚整个迁移 ——「跑过了」和「验过了」是两件事。
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
     WHERE id IN ('20000000-0000-0000-0000-000000001810'::uuid,
                  '20000000-0000-0000-0000-000000001820'::uuid)
       AND menu_type = 3
       AND parent_id = '20000000-0000-0000-0000-000000001018'::uuid
       AND path IS NULL
       AND status = 1
       AND deleted_at IS NULL
       AND auth_code = name;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0417 自检失败：两个出表按钮节点只建成 % 个（码授不出去 = 除超管没人能出表）', n;
    END IF;

    -- ===== 一码一节点：同一个 auth_code 不许出现在多行（重复行会让授权页出现两个同名勾选框）=====
    SELECT count(*) INTO n
      FROM (
            SELECT auth_code
              FROM sys_menu
             WHERE auth_code IN ('business:erp:cashflow-statement:list',
                                 'business:erp:cashflow-statement:export')
               AND deleted_at IS NULL
             GROUP BY auth_code
            HAVING count(*) = 1
           ) one_node;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0417 自检失败：两个码在 sys_menu 里应各恰好 1 行（实际只 % 个码满足）', n;
    END IF;

    -- ===== title 规范：≤12 字、不带标点/说明（menu_invariants 第 6 条）=====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001810'::uuid,
                  '20000000-0000-0000-0000-000000001820'::uuid)
       AND length(title) <= 12
       AND title NOT LIKE '%，%'
       AND title NOT LIKE '%（%'
       AND title NOT LIKE '%。%'
       AND btrim(title) <> '';
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0417 自检失败：出表按钮节点的 title 不合菜单规范（≤12 字、不带标点/说明）';
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
        RAISE EXCEPTION 'V0417 自检失败：「现金流量表」页面下有 % 组重复的 sort_order（本卡用 50 / 60）', n;
    END IF;

    -- ===== 授权：有角色就该授到；角色不存在（空库首启）则跳过 =====
    SELECT count(*) INTO n_roles
      FROM sys_role
     WHERE deleted_at IS NULL
       AND role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    SELECT count(*) INTO n_grant
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id IN ('20000000-0000-0000-0000-000000001810'::uuid,
                          '20000000-0000-0000-0000-000000001820'::uuid)
       AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER');
    IF n_grant <> n_roles * 2 THEN
        RAISE EXCEPTION 'V0417 自检失败：授权数 % ≠ 目标角色数 % × 2 个码（有角色没授到 → 有人点不开报表）',
            n_grant, n_roles;
    END IF;

    RAISE NOTICE 'V0417 自检通过：现金流量表出表 2 个权限码（出表查看 / 出表导出）挂「现金流量表」页面，授权 % 个角色 × 2 码 = % 行',
        n_roles, n_grant;
END $$;
