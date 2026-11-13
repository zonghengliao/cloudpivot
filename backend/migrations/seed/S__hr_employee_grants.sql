-- ============================================================================
-- 人力资源 · 员工自助（EMPLOYEE）4 个 HR 权限点授权 —— 可重复种子（S__ = repeatable）
--
-- 依据：卡 HR-47（t_3389f8aa）「权限矩阵回归（30 码 × 6 角色，含 SELF 只读自己薪资）」
--       实测发现：**全新库上 EMPLOYEE 角色一个 business:hr:* 码都没有**。
--
-- ── 现场（145 / 空库 hr47_drill，2026-11-13 实测）────────────────────────────
--   V0387__hr_menu_perms.up.sql §4 的 EMPLOYEE 段打的是：
--     NOTICE: V0387：角色 EMPLOYEE 不存在 → 跳过它的授权（自助码由
--             S__employee_self_service_permissions 兜）
--   而那句 NOTICE 的兜底**并不存在**：S__employee_self_service_permissions.sql 只授
--   报销域 13 行，一行 business:hr:* 都没有。结果：
--     · 架构 §3.2 ⑤「全员默认拿 business:hr:salary:my + business:hr:leave:create」落空；
--     · 员工在「薪资管理 → 我的薪资」tab 调 GET /business/hr/salary/my 永远 403
--       （报文：无权限访问，需要权限: business:hr:salary:my）；
--     · HR-47 的「SELF 只能看自己」验收在全新库上根本无法验证。
--
-- ── 根因：种子（S__）在版本化迁移**之后**跑 ─────────────────────────────────
--   加载顺序是 system/ → business/ → seed/。EMPLOYEE 角色由 S__ 种，所以 V0387
--   执行时它还不存在（`IF v_role IS NULL THEN RAISE NOTICE …` 分支静默跳过）。
--   老库/生产库因为 EMPLOYEE 早在上一轮种子就跑过（角色已在），V0387 的授权是生效的 ——
--   所以这是**只在全新安装上出现**的缺口，演练库/新客户首装必踩。
--   ⇒ 修在 S__（可重复、每次启动重放、幂等），而不是再加一条版本化迁移：
--     版本化迁移同样早于种子执行，加 V0400 也是同样的 NOTICE 分支。
--
-- ── 为什么是这 4 个（与 V0387 逐 id 对齐，一个不多）──────────────────────────
--   设计 §3.2 ⑤：员工自助只拿「页面外壳 + 两个自助码」，不给 employee:detail /
--   salary:list（那会变成「看全员档案 / 全员薪资」）。故：
--     e000  人力资源（目录，祖先闭包 —— 缺它侧边栏整棵子树被静默丢掉）
--     e003  请假单页      business:hr:leave:page（V0401 起；之前是 :list）
--     e031  新建请假单    business:hr:leave:create
--     e004  薪资管理页    business:hr:salary:page（V0401 起；之前是 :list）
--     e047  我的薪资      business:hr:salary:my
--   与 V0387 的 `emp_hr` 数组**同一个 id 集合**；页面行的码由 V0401 就地改名
--   （id 不变 ⇒ 本种子按 id 授权，一行都不用改）。**不新增权限码、不动菜单行。**
--   ⚠ V0401 之后员工不再持有 `*:list`：自助列表走 `/leave/my`、`/salary/my`
--     （卡 t_0af7ec46；决策见 docs/architecture/PERM-01-页面码与接口码解耦-决策.md）。
--
-- ── 幂等 / 回滚 ────────────────────────────────────────────────────────────
--   · 授权一律 `WHERE NOT EXISTS (…)`：重放 = 0 行；已在（老库/生产）也是 0 行；
--   · 没有 down：S__ 是可重复种子，删掉本文件即等于「不再重放」；已落库的授权属
--     V0387 的设计意图，不该被种子的回滚带走（同批 S__ 文件均无 down）。
--   · 自检：EMPLOYEE 角色在 ⇒ 这 5 行授权必须在，否则 RAISE EXCEPTION（宁可启动失败）。
-- ============================================================================

-- ---------- ① 授权（只用 id，与 V0387 的 emp_hr 数组逐项对齐）----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES
        ('20000000-0000-4000-8000-00000000e000'::uuid),  -- 目录：人力资源
        ('20000000-0000-4000-8000-00000000e003'::uuid),  -- 请假单页      business:hr:leave:list
        ('20000000-0000-4000-8000-00000000e031'::uuid),  -- 新建请假单    business:hr:leave:create
        ('20000000-0000-4000-8000-00000000e004'::uuid),  -- 薪资管理页    business:hr:salary:list
        ('20000000-0000-4000-8000-00000000e047'::uuid)   -- 我的薪资      business:hr:salary:my
     ) AS m(id)
 WHERE r.role_code = 'EMPLOYEE'
   AND r.deleted_at IS NULL
   -- 菜单行必须真的存在（没跑过 V0387 的库不该被本种子插出悬空授权）
   AND EXISTS (SELECT 1 FROM sys_menu x WHERE x.id = m.id AND x.deleted_at IS NULL)
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       );

-- ---------- ② 自检：角色在 ⇒ 5 行授权必须齐（缺一行就启动失败）----------
DO $$
DECLARE
    v_role   UUID;
    v_menus  INT;
    v_grant  INT;
    v_missing TEXT;
BEGIN
    -- HR 菜单没装（没跑过 V0387）或 EMPLOYEE 角色没种 ⇒ 本种子无事可做
    SELECT count(*) INTO v_menus
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND id = ANY (ARRAY[
            '20000000-0000-4000-8000-00000000e000',
            '20000000-0000-4000-8000-00000000e003',
            '20000000-0000-4000-8000-00000000e031',
            '20000000-0000-4000-8000-00000000e004',
            '20000000-0000-4000-8000-00000000e047']::uuid[]);
    IF v_menus <> 5 THEN
        RAISE NOTICE 'S__hr_employee_grants：人力资源菜单未装齐（% / 5 行）→ 跳过自检', v_menus;
        RETURN;
    END IF;

    SELECT id INTO v_role FROM sys_role WHERE role_code = 'EMPLOYEE' AND deleted_at IS NULL LIMIT 1;
    IF v_role IS NULL THEN
        RAISE NOTICE 'S__hr_employee_grants：角色 EMPLOYEE 不存在（应由 S__employee_self_service_permissions 先种）→ 跳过';
        RETURN;
    END IF;

    SELECT count(*) INTO v_grant
      FROM sys_role_menu rm
     WHERE rm.role_id = v_role
       AND rm.menu_id = ANY (ARRAY[
            '20000000-0000-4000-8000-00000000e000',
            '20000000-0000-4000-8000-00000000e003',
            '20000000-0000-4000-8000-00000000e031',
            '20000000-0000-4000-8000-00000000e004',
            '20000000-0000-4000-8000-00000000e047']::uuid[]);
    IF v_grant <> 5 THEN
        SELECT string_agg(m.id::text, ', ') INTO v_missing
          FROM sys_menu m
         WHERE m.id = ANY (ARRAY[
                '20000000-0000-4000-8000-00000000e000',
                '20000000-0000-4000-8000-00000000e003',
                '20000000-0000-4000-8000-00000000e031',
                '20000000-0000-4000-8000-00000000e004',
                '20000000-0000-4000-8000-00000000e047']::uuid[])
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu g WHERE g.role_id = v_role AND g.menu_id = m.id);
        RAISE EXCEPTION
            'S__hr_employee_grants 自检失败：EMPLOYEE 只有 % / 5 个人力资源授权，缺 %（员工自助会 403）',
            v_grant, v_missing;
    END IF;

    RAISE NOTICE 'S__hr_employee_grants：EMPLOYEE 人力资源授权 5 / 5 在位（我的薪资 / 我的请假可自助访问）';
END $$;
