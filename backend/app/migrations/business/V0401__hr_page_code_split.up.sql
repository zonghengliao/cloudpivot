-- ============================================================================
-- V0401：页面显形码与全量接口码解耦 —— 试点「人力资源」5 个页面行
--
-- 卡：t_0af7ec46（平台 · 页面显形码与全量接口码解耦；t_be54535c 的根治项）
-- 决策全文：docs/architecture/PERM-01-页面码与接口码解耦-决策.md
--
-- # 解耦的是什么（一句话）
--   以前：页面行（menu_type=2）的 auth_code 同时是那条**全量列表接口**的码
--         ⇒ 授「页面显形」就等于授「看全量」（EMPLOYEE 因此持有 salary:list / leave:list，
--         HR-47 实测员工能拉全公司工资单）。各模块只能靠在 handler 里人肉加第二道闸兜底。
--   以后：页面行用 **`{模块}:{资源}:page`**（显形码，纯前端判据）；
--         `{模块}:{资源}:list` 挪到**按钮行（menu_type=3）**，只交给全量列表接口。
--         两个码各自独立授予 ⇒ 根因消失，第二道闸可以退休。
--
-- # 迁移策略（决策 2；为什么不是「改码 + 重新勾授权」）
--   ① **页面行原地改名**（`UPDATE sys_menu SET auth_code = …`，id 不变）：
--      `sys_role_menu` 挂的是 menu_id ⇒ 授过页面的角色**一行不丢**，侧边栏不会整片消失。
--   ② **`*:list` 改成新按钮行**：接口码必须仍有一条 sys_menu 行承载，否则
--      N10（注解 → 必须菜单承载）红、且角色无法把「看全量」勾给别人。
--   ③ **同一迁移按设计意图补 `*:list` 授权**（见 §3 的矩阵与理由）：
--      旧库里 `*:list` 是随页面码来的，改名后必须显式补给「本来就该看全量」的角色，
--      否则人事经理 / 财务经理会突然 403（功能静默残废）。
--   ④ **不给 EMPLOYEE 补** —— 那正是本卡要修掉的越权面（员工自助只能走 `/xx/my`）。
--
-- # 为什么不用 shared::permission_alias 做新旧码互扩（重要）
--   别名表是**双向**的（旧码 ⇄ 规范码）。给 `page` 与 `list` 建别名 = 又把两个码绑回
--   一起（持 page 即得 list），本卡的根因会原样复活。故本迁移**不登记别名**。
--   代价与兜底：迁移瞬间**在途 JWT** 里仍是旧快照（EMPLOYEE 的令牌还带 `*:list`），
--   最长持续一个 access token TTL；刷新/重登即按新授权签发。这条写在 ADR §影响面里。
--
-- # 幂等 / 回滚
--   · 改名：`UPDATE … WHERE id = … AND menu_type = 2 AND auth_code = '<旧码>'`（重放 = 0 行）
--   · 新按钮：`INSERT … SELECT … WHERE NOT EXISTS(同 id)` + `ON CONFLICT (id) DO NOTHING`
--   · 授权：`ON CONFLICT (role_id, menu_id) DO NOTHING`
--   · 页面行缺失（客户删过该页）不阻断迁移：该页整组跳过（改名 0 行 ⇒ 不建按钮 ⇒ 不授权）
--   · down：见 V0401__hr_page_code_split.down.sql（撤授权 → 删按钮 → 页面码改回 `*:list`）
--
-- # 自检（§4，任一不符 → 迁移失败）
--   页面行不许还挂着 `*:list`；`:page` 行数 == `:list` 按钮行数；EMPLOYEE 一个 `:list` 都没有；
--   HR_MANAGER 五个 `:list` 全在。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- §0 前置护栏：新码 / 新 id 不许被别的菜单行占着
--     （`ON CONFLICT (id) DO NOTHING` 有个洞：若 e061 已被别的东西占用，
--       INSERT 静默不插，而按 auth_code 的授权段又会命中别的行 —— 所以先点名）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    n      INT;
    sample TEXT;
BEGIN
    SELECT count(*), string_agg(id::text || ' → ' || auth_code, '; ')
      INTO n, sample
      FROM sys_menu
     WHERE id = ANY (ARRAY[
            '20000000-0000-4000-8000-00000000e061','20000000-0000-4000-8000-00000000e062',
            '20000000-0000-4000-8000-00000000e063','20000000-0000-4000-8000-00000000e064',
            '20000000-0000-4000-8000-00000000e065'
           ]::uuid[])
       AND auth_code NOT IN (
            'business:hr:employee:list','business:hr:attendance:list','business:hr:leave:list',
            'business:hr:salary:list','business:hr:salary-item:list'
           );
    IF n > 0 THEN
        RAISE EXCEPTION 'V0401 拒绝执行：% 个预留按钮 id 已被其它权限码占用（%）', n, sample;
    END IF;

    -- 新页面码不许已被别的行占着（否则改名后同码两行，角色树上分不清）
    SELECT count(*), string_agg(id::text, '; ')
      INTO n, sample
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND menu_type = 2
       AND auth_code IN (
            'business:hr:employee:page','business:hr:attendance:page','business:hr:leave:page',
            'business:hr:salary:page','business:hr:salary-item:page'
           )
       AND id <> ALL (ARRAY[
            '20000000-0000-4000-8000-00000000e001','20000000-0000-4000-8000-00000000e002',
            '20000000-0000-4000-8000-00000000e003','20000000-0000-4000-8000-00000000e004',
            '20000000-0000-4000-8000-00000000e005'
           ]::uuid[]);
    IF n > 0 THEN
        RAISE EXCEPTION 'V0401 拒绝执行：% 个 `*:page` 码已被别的页面行占用（%）', n, sample;
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- §1 ① 页面行原地改名：`*:list` → `*:page`（id 不动 ⇒ 已授的授权一行不丢）
--     逐条写（而不是 UPDATE … WHERE auth_code LIKE）是为了让「哪一行换成哪个码」可逐字核对，
--     也让重放时每条都精确命中 0 行。
-- ----------------------------------------------------------------------------
UPDATE sys_menu SET auth_code = 'business:hr:employee:page', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e001'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:employee:list';

UPDATE sys_menu SET auth_code = 'business:hr:attendance:page', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e002'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:attendance:list';

UPDATE sys_menu SET auth_code = 'business:hr:leave:page', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e003'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:leave:list';

UPDATE sys_menu SET auth_code = 'business:hr:salary:page', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e004'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:salary:list';

UPDATE sys_menu SET auth_code = 'business:hr:salary-item:page', updated_at = CURRENT_TIMESTAMP
 WHERE id = '20000000-0000-4000-8000-00000000e005'::uuid
   AND menu_type = 2 AND auth_code = 'business:hr:salary-item:list';

-- ----------------------------------------------------------------------------
-- §2 ② `*:list` 挪到按钮行（menu_type=3，不进侧栏）
--     只在「页面行确实已解耦成 `*:page`」时插入 ⇒ 页面被客户删过 / 改过名的那一组
--     整组跳过，不会留下挂在不存在页面下的孤儿按钮。
--     sort_order 取该父页面下现有按钮之后的空位（AGENTS 约定 12：同父 sort 唯一）。
-- ----------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-4000-8000-00000000e061'::uuid, 'business:hr:employee:list', 3,
       'business:hr:employee:list', NULL, '20000000-0000-4000-8000-00000000e001'::uuid,
       NULL, 100, 1, NULL, '查看全量列表', FALSE, 'business'
 WHERE EXISTS (SELECT 1 FROM sys_menu p
                WHERE p.id = '20000000-0000-4000-8000-00000000e001'::uuid
                  AND p.deleted_at IS NULL
                  AND p.auth_code = 'business:hr:employee:page')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-4000-8000-00000000e062'::uuid, 'business:hr:attendance:list', 3,
       'business:hr:attendance:list', NULL, '20000000-0000-4000-8000-00000000e002'::uuid,
       NULL, 60, 1, NULL, '查看全量列表', FALSE, 'business'
 WHERE EXISTS (SELECT 1 FROM sys_menu p
                WHERE p.id = '20000000-0000-4000-8000-00000000e002'::uuid
                  AND p.deleted_at IS NULL
                  AND p.auth_code = 'business:hr:attendance:page')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-4000-8000-00000000e063'::uuid, 'business:hr:leave:list', 3,
       'business:hr:leave:list', NULL, '20000000-0000-4000-8000-00000000e003'::uuid,
       NULL, 70, 1, NULL, '查看全量列表', FALSE, 'business'
 WHERE EXISTS (SELECT 1 FROM sys_menu p
                WHERE p.id = '20000000-0000-4000-8000-00000000e003'::uuid
                  AND p.deleted_at IS NULL
                  AND p.auth_code = 'business:hr:leave:page')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-4000-8000-00000000e064'::uuid, 'business:hr:salary:list', 3,
       'business:hr:salary:list', NULL, '20000000-0000-4000-8000-00000000e004'::uuid,
       NULL, 80, 1, NULL, '查看全量列表', FALSE, 'business'
 WHERE EXISTS (SELECT 1 FROM sys_menu p
                WHERE p.id = '20000000-0000-4000-8000-00000000e004'::uuid
                  AND p.deleted_at IS NULL
                  AND p.auth_code = 'business:hr:salary:page')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-4000-8000-00000000e065'::uuid, 'business:hr:salary-item:list', 3,
       'business:hr:salary-item:list', NULL, '20000000-0000-4000-8000-00000000e005'::uuid,
       NULL, 40, 1, NULL, '查看全量列表', FALSE, 'business'
 WHERE EXISTS (SELECT 1 FROM sys_menu p
                WHERE p.id = '20000000-0000-4000-8000-00000000e005'::uuid
                  AND p.deleted_at IS NULL
                  AND p.auth_code = 'business:hr:salary-item:page')
ON CONFLICT (id) DO NOTHING;

--   ⚠ 空库首启的已知无害差异：`SUPER_ADMIN` 由**可重复种子** `S__role_data.sql` 创建，
--     而种子在**所有版本化迁移之后**执行 ⇒ 空库上执行本迁移时它还不存在，§3 里它的 5 行
--     授权会被 `JOIN sys_role` 静默跳过。**故意不在种子里补**（种子重放会把管理员在权限页
--     撤销的授予再加回来，V0258 §二·方案 C 已论证过）。功能上无影响：超管的权限是 `*:*:*`
--     通配、菜单也是 `menu_type IN (1,2)` 全量下发，不读 sys_role_menu（`auth::model` 的
--     超管分支）。老库（生产）该角色早已存在，5 行照常落库。
--     其余 4 个角色都先于本迁移存在（`HR_MANAGER` 由 V0387 建，`FINANCE_MANAGER` /
--     `DEPT_LEADER` 由 V0182/V0191/V0206 建），授权必然命中。
--
-- ----------------------------------------------------------------------------
-- §3 ③ 按**设计意图**给角色补 `*:list`（逐条写理由，别照抄「谁有页面就给谁」）
--
--   旧口径（V0387 §4）：`*:list` 是随页面行来的，所以「有页面」≈「有全量」。
--   解耦后必须显式回答「谁**该**看全量」，逐码的答案与依据：
--
--   SUPER_ADMIN     5 个全给 —— 超管本就 `*:*:*` 短路，显式授予只为角色树上能看清范围
--   HR_MANAGER      5 个全给 —— 人事经理的岗位就是维护全量档案 / 考勤 / 请假 / 薪资
--   FINANCE_MANAGER salary:list    —— 财务要看全公司工资单（V0387 已给它 salary 页 + detail/post/export）
--   DEPT_LEADER     employee:list  —— 它本来就有「员工档案页 + employee:detail」，且
--                                    data_scope=1（全部）；解耦后不补会从「能看员工列表」掉成 403（功能残废）
--   EMPLOYEE        **一个都不给** —— 这正是本卡要修的越权面：员工自助只走
--                                    `/business/hr/leave/my`、`/business/hr/salary/my`
--
--   ⚠ DEPT_LEADER **不补 leave:list**（与 employee:list 不同，这里刻意收窄）：
--     它手上只有页面码时，`/leave/page` 的人肉闸把它收窄成「只看本人」；解耦后闸要退休，
--     而它的 data_scope=1 是「全部」⇒ 补了 leave:list 就等于让部门主管看**全公司**请假单，
--     比现状**更宽**。要让它看「本部门请假」，得先给 biz_leave_request 补数据范围裁剪
--     （现在不在 SCOPED_TABLES 里），那是另一张卡的事。本卡不擅自扩大可见面。
--     ⇒ 部门主管的请假入口是「我的请假」(`/leave/my`) + 流程中心的审批详情。
-- ----------------------------------------------------------------------------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM (VALUES
        ('SUPER_ADMIN',    'business:hr:employee:list'),
        ('SUPER_ADMIN',    'business:hr:attendance:list'),
        ('SUPER_ADMIN',    'business:hr:leave:list'),
        ('SUPER_ADMIN',    'business:hr:salary:list'),
        ('SUPER_ADMIN',    'business:hr:salary-item:list'),
        ('HR_MANAGER',     'business:hr:employee:list'),
        ('HR_MANAGER',     'business:hr:attendance:list'),
        ('HR_MANAGER',     'business:hr:leave:list'),
        ('HR_MANAGER',     'business:hr:salary:list'),
        ('HR_MANAGER',     'business:hr:salary-item:list'),
        ('FINANCE_MANAGER','business:hr:salary:list'),
        ('DEPT_LEADER',    'business:hr:employee:list')
       ) AS g(role_code, auth_code)
  JOIN sys_role r ON r.role_code = g.role_code AND r.deleted_at IS NULL
  JOIN sys_menu m ON m.auth_code = g.auth_code AND m.menu_type = 3 AND m.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ----------------------------------------------------------------------------
-- §4 ④ 自检（任一不符 → 迁移失败；宁可启动失败，也不要「页面码还在、接口码没了」）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    own_pages UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e001','20000000-0000-4000-8000-00000000e002',
        '20000000-0000-4000-8000-00000000e003','20000000-0000-4000-8000-00000000e004',
        '20000000-0000-4000-8000-00000000e005'
    ]::uuid[];
    own_list_btns UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e061','20000000-0000-4000-8000-00000000e062',
        '20000000-0000-4000-8000-00000000e063','20000000-0000-4000-8000-00000000e064',
        '20000000-0000-4000-8000-00000000e065'
    ]::uuid[];
    n_page   INT;
    n_btn    INT;
    n_stale  INT;
    n_emp    INT;
    n_hr     INT;
    sample   TEXT;
BEGIN
    SELECT count(*) INTO n_page
      FROM sys_menu
     WHERE id = ANY(own_pages) AND deleted_at IS NULL
       AND auth_code LIKE 'business:hr:%:page';

    -- 4.1 解耦过的页面行不许还挂着 `*:list`（改名的 WHERE 条件失效时会留在这里）
    SELECT count(*), string_agg(name || ' → ' || auth_code, '; ')
      INTO n_stale, sample
      FROM sys_menu
     WHERE id = ANY(own_pages) AND deleted_at IS NULL
       AND auth_code IN (
            'business:hr:employee:list','business:hr:attendance:list','business:hr:leave:list',
            'business:hr:salary:list','business:hr:salary-item:list');
    IF n_stale > 0 THEN
        RAISE EXCEPTION 'V0401 自检失败：% 个页面行仍是 `*:list`（%）', n_stale, sample;
    END IF;

    -- 4.2 页面行与 `:list` 按钮行一一对应（每一页恰好一个全量列表码）
    SELECT count(*) INTO n_btn
      FROM sys_menu
     WHERE id = ANY(own_list_btns) AND deleted_at IS NULL AND menu_type = 3;
    IF n_btn <> n_page THEN
        RAISE EXCEPTION 'V0401 自检失败：`*:page` 页面 % 行，`:list` 按钮 % 行（应相等）', n_page, n_btn;
    END IF;

    -- 4.3 每个 `:list` 按钮都必须挂在它自己的页面下（父级错 = 角色树上找不到）
    SELECT count(*) INTO n_stale
      FROM sys_menu b
      JOIN sys_menu p ON p.id = b.parent_id
     WHERE b.id = ANY(own_list_btns) AND b.deleted_at IS NULL
       AND p.auth_code <> replace(b.auth_code, ':list', ':page');
    IF n_stale > 0 THEN
        RAISE EXCEPTION 'V0401 自检失败：% 个 `:list` 按钮的父页面不是同资源的 `*:page` 行', n_stale;
    END IF;

    -- 4.4 EMPLOYEE 一个 `*:list` 都不许有（本卡的验收口径）
    SELECT count(*) INTO n_emp
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
      JOIN sys_menu m ON m.id = rm.menu_id
     WHERE r.role_code = 'EMPLOYEE' AND r.deleted_at IS NULL
       AND m.id = ANY(own_list_btns);
    IF n_emp > 0 THEN
        RAISE EXCEPTION 'V0401 自检失败：EMPLOYEE 仍持有 % 个全量列表码（越权面未关闭）', n_emp;
    END IF;

    -- 4.5 HR_MANAGER 五个 `:list` 必须齐（缺一个 = 人事经理功能静默残废）
    SELECT count(*) INTO n_hr
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
     WHERE r.role_code = 'HR_MANAGER' AND r.deleted_at IS NULL
       AND rm.menu_id = ANY(own_list_btns);
    IF EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL)
       AND n_hr <> n_btn THEN
        RAISE EXCEPTION 'V0401 自检失败：HR_MANAGER 只有 % / % 个全量列表码', n_hr, n_btn;
    END IF;

    RAISE NOTICE 'V0401：页面码已解耦 % 行（`*:page`），全量列表码 % 行（`*:list` 按钮）；EMPLOYEE 持有 0 个',
        n_page, n_btn;
END $$;
