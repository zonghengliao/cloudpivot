-- ============================================================================
-- V0387.down.sql —— 回滚人力资源的菜单 / 按钮权限 / HR_MANAGER 角色与授权
--                  （对应 V0387__hr_menu_perms）
--
-- 顺序（外键方向决定）：授权 → 按钮(menu_type=3) → 页面(menu_type=2) → 目录(menu_type=1) → 角色
--   `sys_role_menu.menu_id` 与 `sys_role_menu.role_id` 都有外键，先删引用侧。
--
-- 三道护栏（宁可 down 失败也不留裂树 / 不吞别人的数据）：
--   ① 目录/页面下如果挂出了**本迁移之外**的子菜单（有人在其下加了自定义页面/按钮）→ 拒绝执行；
--   ② 30 个按钮下如果挂了任何子节点 → 拒绝执行（按钮不该有子节点，说明数据被手工改坏）；
--   ③ HR_MANAGER 角色只在本迁移**建的那个 id** 上、且没有用户绑定、且没有被授过本迁移之外的
--      菜单时才删除；否则只删本迁移的 36 条授权、**保留角色**并打 NOTICE（客户已经用它做了
--      别的事，down 不该顺手删掉）。
--
-- 不动的东西：sys_menu 其它迁移的行 / sys_role 其它角色 / biz_* 业务表（V0383 的 10 张表不在
--   本迁移范围内）。授权只按**本迁移的 36 个 menu_id** 删，不做 role_code 通配 —— 那样会误伤
--   同码的其它菜单授权。
--
-- 末尾零残留断言：36 个 menu_id 必须 0 行（否则 down 算失败）。
-- ============================================================================

DO $$
DECLARE
    own_dir     UUID := '20000000-0000-4000-8000-00000000e000'::uuid;
    own_role_id UUID := '20000000-0000-4000-8000-0000000f0016'::uuid;
    own_pages   UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e001','20000000-0000-4000-8000-00000000e002',
        '20000000-0000-4000-8000-00000000e003','20000000-0000-4000-8000-00000000e004',
        '20000000-0000-4000-8000-00000000e005'
    ]::uuid[];
    own_btns    UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e011','20000000-0000-4000-8000-00000000e012',
        '20000000-0000-4000-8000-00000000e013','20000000-0000-4000-8000-00000000e014',
        '20000000-0000-4000-8000-00000000e015','20000000-0000-4000-8000-00000000e016',
        '20000000-0000-4000-8000-00000000e017','20000000-0000-4000-8000-00000000e018',
        '20000000-0000-4000-8000-00000000e019',
        '20000000-0000-4000-8000-00000000e021','20000000-0000-4000-8000-00000000e022',
        '20000000-0000-4000-8000-00000000e023','20000000-0000-4000-8000-00000000e024',
        '20000000-0000-4000-8000-00000000e025',
        '20000000-0000-4000-8000-00000000e031','20000000-0000-4000-8000-00000000e032',
        '20000000-0000-4000-8000-00000000e033','20000000-0000-4000-8000-00000000e034',
        '20000000-0000-4000-8000-00000000e035','20000000-0000-4000-8000-00000000e036',
        '20000000-0000-4000-8000-00000000e041','20000000-0000-4000-8000-00000000e042',
        '20000000-0000-4000-8000-00000000e043','20000000-0000-4000-8000-00000000e044',
        '20000000-0000-4000-8000-00000000e045','20000000-0000-4000-8000-00000000e046',
        '20000000-0000-4000-8000-00000000e047',
        '20000000-0000-4000-8000-00000000e051','20000000-0000-4000-8000-00000000e052',
        '20000000-0000-4000-8000-00000000e053'
    ]::uuid[];
    all_ids     UUID[];
    n_extra     INT;
    sample      TEXT;
    n_grant     INT;
    n_menu      INT;
    n_left      INT;
    n_users     INT;
    n_outside   INT;
    role_kept   BOOLEAN := FALSE;
BEGIN
    all_ids := ARRAY[own_dir] || own_pages || own_btns;

    -- ① 目录/页面下不许有本迁移之外的子菜单
    SELECT count(*), string_agg(c.path || ' (' || c.id::text || ')', '; ')
      INTO n_extra, sample
      FROM sys_menu c
     WHERE c.parent_id = ANY(ARRAY[own_dir] || own_pages)
       AND NOT (c.id = ANY(own_pages || own_btns));
    IF n_extra > 0 THEN
        RAISE EXCEPTION 'V0387.down 拒绝执行：人力资源目录/页面下存在 % 个本迁移之外的子菜单（%）—— 请先人工处理',
            n_extra, sample;
    END IF;

    -- ② 按钮下不许有子节点
    SELECT count(*), string_agg(c.id::text, '; ')
      INTO n_extra, sample
      FROM sys_menu c
     WHERE c.parent_id = ANY(own_btns);
    IF n_extra > 0 THEN
        RAISE EXCEPTION 'V0387.down 拒绝执行：人力资源按钮下存在 % 个子节点（%）—— 数据被手工改坏，请先人工处理',
            n_extra, sample;
    END IF;

    -- ③ 角色删除条件（在删菜单之前算，因为"是否被授过本迁移之外的菜单"要能看见）
    IF EXISTS (SELECT 1 FROM sys_role WHERE id = own_role_id) THEN
        SELECT count(*) INTO n_outside
          FROM sys_role_menu rm
         WHERE rm.role_id = own_role_id AND NOT (rm.menu_id = ANY(all_ids));
        SELECT count(*) INTO n_users
          FROM sys_user_role ur WHERE ur.role_id = own_role_id;
        IF n_outside > 0 OR n_users > 0 THEN
            role_kept := TRUE;
            RAISE NOTICE 'V0387.down：HR_MANAGER 有 % 条本迁移之外的菜单授权、% 个绑定用户 → 只删本迁移的 36 条授权，保留角色',
                n_outside, n_users;
        END IF;
    END IF;

    -- ④ 删授权（只按本迁移的 36 个 menu_id）
    DELETE FROM sys_role_menu WHERE menu_id = ANY(all_ids);
    GET DIAGNOSTICS n_grant = ROW_COUNT;

    -- ⑤ 删按钮 → 页面 → 目录（硬删：这些行是本迁移种的，软删会留下占位 path 挡住再次 up）
    DELETE FROM sys_menu WHERE id = ANY(own_btns);
    GET DIAGNOSTICS n_menu = ROW_COUNT;
    DELETE FROM sys_menu WHERE id = ANY(own_pages);
    GET DIAGNOSTICS n_left = ROW_COUNT;
    n_menu := n_menu + n_left;
    DELETE FROM sys_menu WHERE id = own_dir;
    GET DIAGNOSTICS n_left = ROW_COUNT;
    n_menu := n_menu + n_left;

    -- ⑥ 删角色（仅本迁移建的那个 id，且未被客户复用）
    IF NOT role_kept THEN
        DELETE FROM sys_role WHERE id = own_role_id AND role_code = 'HR_MANAGER';
        GET DIAGNOSTICS n_users = ROW_COUNT;
        IF n_users > 0 THEN
            RAISE NOTICE 'V0387.down：已删除 HR_MANAGER 角色';
        END IF;
    END IF;

    -- ⑦ 零残留断言
    SELECT count(*) INTO n_left FROM sys_menu WHERE id = ANY(all_ids);
    IF n_left > 0 THEN
        RAISE EXCEPTION 'V0387.down 失败：仍残留 % 条人力资源菜单行', n_left;
    END IF;
    SELECT count(*) INTO n_left FROM sys_role_menu WHERE menu_id = ANY(all_ids);
    IF n_left > 0 THEN
        RAISE EXCEPTION 'V0387.down 失败：仍残留 % 条人力资源菜单授权', n_left;
    END IF;

    RAISE NOTICE 'V0387.down：已删除 % 条授权、% 条菜单（1 目录 + 5 页面 + 30 按钮）%',
        n_grant, n_menu, CASE WHEN role_kept THEN '；HR_MANAGER 角色按护栏保留' ELSE '' END;
END $$;
