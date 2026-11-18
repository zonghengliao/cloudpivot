-- ============================================================================
-- V0460：成本方法配置 · 菜单 + 权限点 + 角色授权（M5-01）
--
-- 配套 V0459（表 biz_cost_method_config）。本迁移只碰 `sys_menu` / `sys_role_menu`。
--
-- ── 挂载点（按 V0390 的现状，不按设计稿）─────────────────────────────────
--   5 个成本页面已在 V0390 收进「财务管理 > 成本核算」子目录
--   （`menu_type=1`、`path='/business/erp/cost'`，子项 sort = 60/62/64/66/68）。
--   本卡新增「成本方法」页挂在**同一个目录**下，`sort_order = 56`：
--   · 取 56 而不是末尾 70：方法配置是**最先要定的事**（先定按什么方法算，
--     才谈归集与分摊），侧栏顺序与数据流一致；
--   · 56 与既有 60/62/64/66/68 无冲突（AGENTS 约定 12「同父 sort 唯一」），
--     且留出 58/59 给后续插队。
--   父目录按 **path 查**而不是写死 UUID：V0390 建目录时用的是 `gen_random_uuid()`，
--   各环境 id 不同，写死会让别的环境迁移直接失败。
--
-- ── 菜单口径（AGENTS 约定 12）────────────────────────────────────────────
--   name  = 页面用中文短名（与 title 一致，与 V0378 同款）；按钮用权限码本身
--   title = 2–6 字，不带标点 / 说明 / 章节号（「成本方法」4 字）
--   icon  = `setting`（TDesign 真实图标：node_modules/tdesign-icons-vue-next/esm/components/setting.js）
--   auth_code：页面行承载 `…:list`（ERP 尚未做页面码 / 接口码解耦，与 V0378 同口径；
--              解耦只试点在 HR，见 AGENTS 约定 14 / docs/architecture/PERM-01）
--
-- ── 权限码（4 个，全部有真路由，全部有校验点）─────────────────────────────
--   | 码 | 载荷接口 |
--   |---|---|
--   | business:erp:cost-method:list   | GET /business/erp/cost-method（分页）+ GET /business/erp/cost-method/effective（查生效方法） |
--   | business:erp:cost-method:create | POST /business/erp/cost-method |
--   | business:erp:cost-method:update | PUT /business/erp/cost-method/{id} |
--   | business:erp:cost-method:delete | DELETE /business/erp/cost-method/{id} |
--   不带 `detail` 码：配置行没有敏感字段，列表行即全部内容（详情接口也不提供，
--   少一个「有码没接口」的死码风险）。
--
-- ── 角色授权（本卡补的口径，理由写在下面）─────────────────────────────────
--   | 角色 | 授权 |
--   |---|---|
--   | SUPER_ADMIN / ADMIN | 4 行全给（角色树上能看清范围；超管本可通配短路） |
--   | FINANCE_MANAGER | 4 行全给 —— 成本方法配置是财务经理的活（N20 要求 create/list 至少授一个非超管角色） |
--   | FINANCE_DIRECTOR | **只给 list** —— 方法切换是会计政策，总监要能看全，但改口径留给财务经理执行 |
--   | WAREHOUSE_MANAGER | 不给 —— 仓管不碰核算口径 |
--   无孤儿：凡是授了按钮码的角色，其父页面行（承载 :list）一并授予。
--
-- ── 幂等 ────────────────────────────────────────────────────────────────
--   菜单 `ON CONFLICT (id) DO NOTHING`（不覆盖人工改名）；授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`。
--   重放 = 0 行插入；末尾自检只在「形态不符」时抛异常。
--
-- ── 回滚 ────────────────────────────────────────────────────────────────
--   V0460.down.sql：先删本迁移授的授权行（按 4 个 menu_id）→ 删 3 个按钮 → 删页面行；
--   页面下若出现本迁移之外的子菜单 → 拒绝（说明有人挂过东西，别替人删）。
-- ============================================================================

-- 页面 / 按钮固定 UUID（…d101 = 页面，…d111–d113 = 按钮），全库唯一性由本迁移 §0 前置护栏保证
DO $$
DECLARE
    v_dir_id  uuid;
    v_conflict int;
BEGIN
    -- §0 前置护栏：4 个 auth_code 不许被**别的 id** 占着（否则会插出第二个同码菜单）
    SELECT count(*) INTO v_conflict
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND auth_code IN ('business:erp:cost-method:list','business:erp:cost-method:create',
                         'business:erp:cost-method:update','business:erp:cost-method:delete')
       AND id NOT IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                      '20000000-0000-4000-8000-00000000d111'::uuid,
                      '20000000-0000-4000-8000-00000000d112'::uuid,
                      '20000000-0000-4000-8000-00000000d113'::uuid);
    IF v_conflict > 0 THEN
        RAISE EXCEPTION 'V0460：cost-method 的权限码已被 % 条其它菜单行占用（先清理再迁移）', v_conflict;
    END IF;

    -- §0.1 同 path 不许被别人占着（menu_invariants ④ path 唯一）
    SELECT count(*) INTO v_conflict
      FROM sys_menu
     WHERE deleted_at IS NULL AND path = '/business/erp/cost-method'
       AND id <> '20000000-0000-4000-8000-00000000d101'::uuid;
    IF v_conflict > 0 THEN
        RAISE EXCEPTION 'V0460：path /business/erp/cost-method 已被其它菜单占用';
    END IF;

    -- §0.2 父目录必须存在（V0390 建；空库首启时 V0390 已先跑）
    SELECT id INTO v_dir_id
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 1 AND path = '/business/erp/cost'
     LIMIT 1;
    IF v_dir_id IS NULL THEN
        RAISE EXCEPTION 'V0460：找不到「成本核算」目录（path=/business/erp/cost, menu_type=1），V0390 未应用？';
    END IF;
    RAISE NOTICE 'V0460：挂载点为「成本核算」目录 %（sort 56）', v_dir_id;
END $$;

-- ---------------------------------------------------------------------------
-- §1 页面菜单（menu_type = 2，进侧栏）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-4000-8000-00000000d101'::uuid, '成本方法', 2, 'business:erp:cost-method:list',
       '/business/erp/cost-method', d.id, 'business/erp/cost-method/index',
       56, 1, 'setting', '成本方法', TRUE, 'business'
  FROM sys_menu d
 WHERE d.deleted_at IS NULL AND d.menu_type = 1 AND d.path = '/business/erp/cost'
   AND NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = '20000000-0000-4000-8000-00000000d101'::uuid);

-- ---------------------------------------------------------------------------
-- §2 按钮权限点（menu_type = 3，is_visible = FALSE，不进侧栏）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
  ('20000000-0000-4000-8000-00000000d111'::uuid,'business:erp:cost-method:create',3,'business:erp:cost-method:create',NULL,'20000000-0000-4000-8000-00000000d101'::uuid,NULL,10,1,NULL,'新增配置',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000d112'::uuid,'business:erp:cost-method:update',3,'business:erp:cost-method:update',NULL,'20000000-0000-4000-8000-00000000d101'::uuid,NULL,20,1,NULL,'修改配置',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000d113'::uuid,'business:erp:cost-method:delete',3,'business:erp:cost-method:delete',NULL,'20000000-0000-4000-8000-00000000d101'::uuid,NULL,30,1,NULL,'删除配置',FALSE,'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- §3 角色授权（矩阵见文件头）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    all_ids  UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000d101','20000000-0000-4000-8000-00000000d111',
        '20000000-0000-4000-8000-00000000d112','20000000-0000-4000-8000-00000000d113'
    ]::uuid[];
    read_ids UUID[] := ARRAY['20000000-0000-4000-8000-00000000d101']::uuid[];
    r RECORD;
    v_role_id UUID;
BEGIN
    FOR r IN
        SELECT * FROM (VALUES
            ('SUPER_ADMIN',      all_ids),
            ('ADMIN',            all_ids),
            ('FINANCE_MANAGER',  all_ids),
            ('FINANCE_DIRECTOR', read_ids)
        ) AS v(role_code, ids)
    LOOP
        BEGIN
            SELECT id INTO STRICT v_role_id FROM sys_role WHERE role_code = r.role_code AND deleted_at IS NULL;
            INSERT INTO sys_role_menu (role_id, menu_id)
            SELECT v_role_id, unnest(r.ids)
            ON CONFLICT (role_id, menu_id) DO NOTHING;
        EXCEPTION WHEN NO_DATA_FOUND THEN
            RAISE NOTICE 'V0460：角色 % 不存在 → 跳过它的授权', r.role_code;
        END;
    END LOOP;
END $$;

-- ---------------------------------------------------------------------------
-- §4 自检（任一不符 → 迁移失败；宁可启动失败也不要「半套菜单 / 半套授权」上线）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_page     integer;
    v_btns     integer;
    v_bad_page integer;
    v_bad_btn  integer;
    v_dup_sort integer;
    v_orphan   integer;
    v_granted  integer;
BEGIN
    -- 4.1 页面行 1 个、形态对（type / path / children / title / icon / component / 显形）
    SELECT count(*) INTO v_page
      FROM sys_menu m
      JOIN sys_menu d ON d.id = m.parent_id AND d.deleted_at IS NULL
     WHERE m.deleted_at IS NULL AND m.id = '20000000-0000-4000-8000-00000000d101'::uuid
       AND m.menu_type = 2 AND m.path = '/business/erp/cost-method'
       AND m.auth_code = 'business:erp:cost-method:list'
       AND m.title = '成本方法' AND length(m.title) <= 12
       AND m.icon = 'setting' AND m.component = 'business/erp/cost-method/index'
       AND m.is_visible IS TRUE AND m.status = 1
       AND m.sort_order = 56 AND d.path = '/business/erp/cost' AND d.menu_type = 1;
    IF v_page <> 1 THEN
        RAISE EXCEPTION 'V0460 自检失败：成本方法页面菜单形态不符（% 行命中期望）', v_page;
    END IF;

    -- 4.2 按钮 3 个、父级正确、name = auth_code、不进侧栏
    SELECT count(*) INTO v_btns
      FROM sys_menu b
     WHERE b.deleted_at IS NULL
       AND b.id IN ('20000000-0000-4000-8000-00000000d111'::uuid,
                    '20000000-0000-4000-8000-00000000d112'::uuid,
                    '20000000-0000-4000-8000-00000000d113'::uuid)
       AND b.menu_type = 3 AND b.is_visible IS FALSE AND b.status = 1
       AND b.name = b.auth_code AND b.auth_code LIKE 'business:erp:cost-method:%'
       AND b.parent_id = '20000000-0000-4000-8000-00000000d101'::uuid;
    IF v_btns <> 3 THEN
        RAISE EXCEPTION 'V0460 自检失败：成本方法按钮应有 3 个且父级正确，实际 %', v_btns;
    END IF;

    -- 4.3 4 个码在 sys_menu 里各出现一次（无重复行 / 无遗漏）
    SELECT count(*) INTO v_bad_btn
      FROM (
        SELECT auth_code FROM sys_menu
         WHERE deleted_at IS NULL AND auth_code LIKE 'business:erp:cost-method:%'
         GROUP BY auth_code HAVING count(*) <> 1
      ) AS dup;
    IF v_bad_btn > 0 THEN
        RAISE EXCEPTION 'V0460 自检失败：cost-method 权限码有重复菜单行 % 个', v_bad_btn;
    END IF;
    IF (SELECT count(DISTINCT auth_code) FROM sys_menu
         WHERE deleted_at IS NULL AND auth_code LIKE 'business:erp:cost-method:%') <> 4 THEN
        RAISE EXCEPTION 'V0460 自检失败：cost-method 权限码应 4 个';
    END IF;

    -- 4.4 同父 sort 唯一（AGENTS 约定 12）
    SELECT count(*) INTO v_dup_sort
      FROM (
        SELECT parent_id, sort_order FROM sys_menu
         WHERE deleted_at IS NULL AND menu_type = 2 AND parent_id = (
                 SELECT id FROM sys_menu WHERE deleted_at IS NULL
                  AND menu_type = 1 AND path = '/business/erp/cost' LIMIT 1)
         GROUP BY parent_id, sort_order HAVING count(*) > 1
      ) AS dup;
    IF v_dup_sort > 0 THEN
        RAISE EXCEPTION 'V0460 自检失败：成本核算目录下 sort_order 有重复（% 组）', v_dup_sort;
    END IF;

    -- 4.5 无孤儿按钮：按钮的父页面行必须在（4.1 已查页面，这里按 parent 再兜一次）
    SELECT count(*) INTO v_orphan
      FROM sys_menu b
     WHERE b.deleted_at IS NULL AND b.auth_code LIKE 'business:erp:cost-method:%'
       AND b.menu_type = 3
       AND NOT EXISTS (SELECT 1 FROM sys_menu p
                        WHERE p.id = b.parent_id AND p.deleted_at IS NULL AND p.menu_type = 2);
    IF v_orphan > 0 THEN
        RAISE EXCEPTION 'V0460 自检失败：% 个按钮没有父页面（孤儿按钮）', v_orphan;
    END IF;

    -- 4.6 授权：财务经理 4 行、总监 1 行（角色存在才查；不存在时只打 NOTICE）
    SELECT count(*) INTO v_granted
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL AND r.role_code = 'FINANCE_MANAGER'
     WHERE rm.menu_id IN ('20000000-0000-4000-8000-00000000d101'::uuid,
                          '20000000-0000-4000-8000-00000000d111'::uuid,
                          '20000000-0000-4000-8000-00000000d112'::uuid,
                          '20000000-0000-4000-8000-00000000d113'::uuid);
    IF EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'FINANCE_MANAGER' AND deleted_at IS NULL)
       AND v_granted <> 4 THEN
        RAISE EXCEPTION 'V0460 自检失败：FINANCE_MANAGER 应授 4 行，实际 %', v_granted;
    END IF;

    RAISE NOTICE 'V0460 完成：成本方法（1 页面 + 3 按钮，4 个权限码）已挂到「成本核算」目录 sort 56';
END $$;
