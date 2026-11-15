-- ============================================================================
-- V0378：成本核算升级 · 菜单 + 权限点 + 角色授权（ARCH-9 §6.1 / §5.3）
--
-- 卡：t_191cf205（COST-B1-7【成本】迁移 V0376/0377/0379）—— 拆卡清单 §8.1 的 B1-7 第一件
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md
--       §6.1 页面清单（6 页 + 挂载点 + sort + title）、§5.3 权限码清单（20 个按钮码）、
--       §7.1 迁移清单（设计稿把这块叫 V0376__cost_menu_and_perm）、
--       以及设计阶段真库实证脚本 docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql §三 ④。
--
-- # 号段为什么是 V0378，而不是卡面的 V0376
--   V0375/V0376 = B1-5（出库加列 / 入库 PRODUCTION），V0377 = B1-6（科目翻末级），
--   V0372/0373/0374 = B1-3/B1-4。nik 迁移加载器对版本号做**跨目录全局唯一**校验
--   （backend/vendor/nik-db/src/migrate/loader.rs），重号会让服务启动直接失败（AGENTS 约定 6）。
--   故 B1-7 的三件顺延为 V0378（本卡）/ V0379（号段字典）/ V0380（批号回填）。
--   ⇒ 设计稿 §7.1 里排 V0378 的 `cost_flow_def`（B3-2 审批流）需从 **V0381** 起。
--
-- # 本迁移只碰 sys_menu / sys_role_menu，不动任何业务表
--   页面指向的 component（business/erp/cost-*）由 B1-11 / B1-12 / B2-13 等前端卡落地；
--   菜单先种下不影响运行：路由未注册时侧栏点进去是 404，等前端卡接上即通。
--   这样排是为了让 B1 批次先过 `scripts/verify/menu_invariants.sh`，避免"页面先有、菜单后在"
--   造成角色授权树上缺分支。
--
-- # 挂载点（真库实测，不是猜的）
--   财务管理 = 20000000-0000-0000-0000-000000000e02（子项 menu_type=2 已占 10/20/21/22/30/40/45/46/47/48/49）
--   仓库管理 = 20000000-0000-0000-0000-000000000e01（子项已占 5/10/20/30/40/50/60/70/80/90）
--
--   ★ 偏差一：批次管理设计写「仓库管理 sort 80」，但 e01 下 **sort 80 已被出库单占用**
--     （`/business/erp/stock-out`），90 = 盘点单。按 AGENTS 约定 12「同父 sort 必须唯一」，
--     本迁移取 **95**（末尾，紧接盘点单）。纯顺序差异，不影响任何功能。
--   ★ 财务管理下的 60/62/64/66/68 与现有子项（…45/46/47/48/49）无冲突，按设计原值。
--
-- # 菜单口径（AGENTS 约定 12）
--   name  = 短标识（页面用中文短名，与 title 一致；按钮用权限码本身，与既有 ERP 按钮行同款）
--   title = 侧边栏显示名，2-6 字，不带标点/说明/章节号
--   icon  = TDesign 真实图标名（本卡 6 个逐个 `ls node_modules/tdesign-icons-vue-next/esm/components/<n>.js` 实证存在）
--
-- # 角色授权矩阵（设计 §7.1 点名 5 个角色；矩阵本身是本卡补的口径，写在下面 §3）
--   设计只给了角色清单、没给逐码矩阵。本卡按"谁的活"落矩阵，并遵守两条硬约束：
--     ① 无孤儿：授了按钮码，其父页面的 list 码必须也授给同一角色（V0356 §二 的口径 +
--        check.sh 的 `orphan_button_gate`）；
--     ② 不越权：红冲结算（`cost-period:reverse`）是"已结转金额倒回去"的最高危动作，
--        只给 FINANCE_DIRECTOR —— 与设计 §4「反结账（财务总监）」同口径。
--   授权是"管理决策"：客户日后可在「角色管理」页面自行调整，本迁移只给一套能开工的默认值。
--
-- # 幂等
--   26 条菜单用固定 UUID + `ON CONFLICT (id) DO NOTHING`（不覆盖人工改名）；
--   授权用 `ON CONFLICT (role_id, menu_id) DO NOTHING`（sys_role_menu 上有 UNIQUE 约束）。
--   重放 = 0 行插入。
--
-- # 回滚
--   V0378.down.sql：先删本迁移授的授权行（按 26 个 menu_id，不按 role 通配）→ 删 20 个按钮
--   → 删 6 个页面。带两道护栏：页面下出现"本迁移之外的子菜单"、或按钮下出现子节点 → 拒绝。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- §0 前置护栏：6 个 path / 26 个 auth_code 不许被**别的 id** 占着
--     只靠 `ON CONFLICT (id) DO NOTHING` 有个洞：客户若自建过 `/business/erp/cost-object`
--     （不同 id），我们会再插一条同 path 的菜单，menu_invariants ④（path 唯一）立刻红，
--     而红的位置在迁移之外，排查成本高。这里提前点名并停住。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    own_ids UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000c001','20000000-0000-4000-8000-00000000c002',
        '20000000-0000-4000-8000-00000000c003','20000000-0000-4000-8000-00000000c004',
        '20000000-0000-4000-8000-00000000c005','20000000-0000-4000-8000-00000000c006',
        '20000000-0000-4000-8000-00000000c101','20000000-0000-4000-8000-00000000c102',
        '20000000-0000-4000-8000-00000000c103','20000000-0000-4000-8000-00000000c104',
        '20000000-0000-4000-8000-00000000c111','20000000-0000-4000-8000-00000000c112',
        '20000000-0000-4000-8000-00000000c113','20000000-0000-4000-8000-00000000c114',
        '20000000-0000-4000-8000-00000000c121','20000000-0000-4000-8000-00000000c122',
        '20000000-0000-4000-8000-00000000c131','20000000-0000-4000-8000-00000000c132',
        '20000000-0000-4000-8000-00000000c133','20000000-0000-4000-8000-00000000c134',
        '20000000-0000-4000-8000-00000000c141','20000000-0000-4000-8000-00000000c142',
        '20000000-0000-4000-8000-00000000c143','20000000-0000-4000-8000-00000000c144',
        '20000000-0000-4000-8000-00000000c151','20000000-0000-4000-8000-00000000c152'
    ]::uuid[];
    own_paths TEXT[] := ARRAY[
        '/business/erp/cost-object','/business/erp/cost-labor','/business/erp/cost-overhead',
        '/business/erp/cost-alloc-rule','/business/erp/cost-period','/business/erp/lot'
    ];
    n_path INT;
    n_code INT;
    sample TEXT;
BEGIN
    SELECT count(*), string_agg(path || ' → ' || id::text, '; ')
      INTO n_path, sample
      FROM sys_menu
     WHERE deleted_at IS NULL AND path = ANY(own_paths) AND NOT (id = ANY(own_ids));
    IF n_path > 0 THEN
        RAISE EXCEPTION 'V0378 拒绝执行：% 条成本页面 path 已被其它菜单占用（%）。请先处理同名页面再升级',
            n_path, sample;
    END IF;

    SELECT count(*), string_agg(auth_code || ' → ' || id::text, '; ')
      INTO n_code, sample
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND auth_code IN (SELECT auth_code FROM sys_menu WHERE id = ANY(own_ids))
       AND NOT (id = ANY(own_ids));
    IF n_code > 0 THEN
        RAISE EXCEPTION 'V0378 拒绝执行：% 条成本权限码已被其它菜单占用（%）。请先处理同码权限点再升级',
            n_code, sample;
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- §1 页面菜单（menu_type = 2，进侧栏）
--     财务管理 e02：sort 60/62/64/66/68（成本对象 → 工时 → 费用 → 分摊规则 → 成本核算，
--     顺序即数据流：先能建对象 → 才能录工时/费用 → 才能配规则 → 才能算）
--     仓库管理 e01：sort 95（批次管理；设计写 80 与出库单撞，见文件头偏差一）
-- ----------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
  ('20000000-0000-4000-8000-00000000c001'::uuid, '成本对象', 2, 'business:erp:cost-object:list',
   '/business/erp/cost-object', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-object/index', 60, 1, 'layers', '成本对象', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c002'::uuid, '工时台账', 2, 'business:erp:cost-labor:list',
   '/business/erp/cost-labor', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-labor/index', 62, 1, 'time', '工时台账', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c003'::uuid, '费用台账', 2, 'business:erp:cost-overhead:list',
   '/business/erp/cost-overhead', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-overhead/index', 64, 1, 'money', '费用台账', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c004'::uuid, '分摊规则', 2, 'business:erp:cost-alloc-rule:list',
   '/business/erp/cost-alloc-rule', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-alloc-rule/index', 66, 1, 'share', '分摊规则', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c005'::uuid, '成本核算', 2, 'business:erp:cost-period:list',
   '/business/erp/cost-period', '20000000-0000-0000-0000-000000000e02'::uuid,
   'business/erp/cost-period/index', 68, 1, 'calculator', '成本核算', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000c006'::uuid, '批次管理', 2, 'business:erp:lot:list',
   '/business/erp/lot', '20000000-0000-0000-0000-000000000e01'::uuid,
   'business/erp/lot/index', 95, 1, 'barcode', '批次管理', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ----------------------------------------------------------------------------
-- §2 按钮权限点（menu_type = 3，is_visible = FALSE，不进侧栏）
--     20 个，与 §5.3 权限码清单一一对应；name = auth_code（与既有 ERP 按钮行同款，
--     便于「角色-菜单」授权树里一眼看出授的是哪个码）。
-- ----------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
  -- 成本对象（4）
  ('20000000-0000-4000-8000-00000000c101'::uuid,'business:erp:cost-object:create',3,'business:erp:cost-object:create',NULL,'20000000-0000-4000-8000-00000000c001'::uuid,NULL,10,1,NULL,'新建成本对象',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c102'::uuid,'business:erp:cost-object:update',3,'business:erp:cost-object:update',NULL,'20000000-0000-4000-8000-00000000c001'::uuid,NULL,20,1,NULL,'修改成本对象',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c103'::uuid,'business:erp:cost-object:delete',3,'business:erp:cost-object:delete',NULL,'20000000-0000-4000-8000-00000000c001'::uuid,NULL,30,1,NULL,'删除成本对象',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c104'::uuid,'business:erp:cost-object:close', 3,'business:erp:cost-object:close', NULL,'20000000-0000-4000-8000-00000000c001'::uuid,NULL,40,1,NULL,'关批',FALSE,'business'),
  -- 工时台账（4）
  ('20000000-0000-4000-8000-00000000c111'::uuid,'business:erp:cost-labor:create',  3,'business:erp:cost-labor:create', NULL,'20000000-0000-4000-8000-00000000c002'::uuid,NULL,10,1,NULL,'新增工时',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c112'::uuid,'business:erp:cost-labor:update',  3,'business:erp:cost-labor:update', NULL,'20000000-0000-4000-8000-00000000c002'::uuid,NULL,20,1,NULL,'修改工时',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c113'::uuid,'business:erp:cost-labor:delete',  3,'business:erp:cost-labor:delete', NULL,'20000000-0000-4000-8000-00000000c002'::uuid,NULL,30,1,NULL,'删除工时',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c114'::uuid,'business:erp:cost-labor:import',  3,'business:erp:cost-labor:import', NULL,'20000000-0000-4000-8000-00000000c002'::uuid,NULL,40,1,NULL,'导入工时',FALSE,'business'),
  -- 费用台账（2）
  ('20000000-0000-4000-8000-00000000c121'::uuid,'business:erp:cost-overhead:create',3,'business:erp:cost-overhead:create',NULL,'20000000-0000-4000-8000-00000000c003'::uuid,NULL,10,1,NULL,'新增费用',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c122'::uuid,'business:erp:cost-overhead:delete',3,'business:erp:cost-overhead:delete',NULL,'20000000-0000-4000-8000-00000000c003'::uuid,NULL,20,1,NULL,'删除费用',FALSE,'business'),
  -- 分摊规则（4）
  ('20000000-0000-4000-8000-00000000c131'::uuid,'business:erp:cost-alloc-rule:create',3,'business:erp:cost-alloc-rule:create',NULL,'20000000-0000-4000-8000-00000000c004'::uuid,NULL,10,1,NULL,'新增规则',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c132'::uuid,'business:erp:cost-alloc-rule:update',3,'business:erp:cost-alloc-rule:update',NULL,'20000000-0000-4000-8000-00000000c004'::uuid,NULL,20,1,NULL,'修改规则',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c133'::uuid,'business:erp:cost-alloc-rule:delete',3,'business:erp:cost-alloc-rule:delete',NULL,'20000000-0000-4000-8000-00000000c004'::uuid,NULL,30,1,NULL,'删除规则',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c134'::uuid,'business:erp:cost-alloc-rule:preview',3,'business:erp:cost-alloc-rule:preview',NULL,'20000000-0000-4000-8000-00000000c004'::uuid,NULL,40,1,NULL,'分摊试算',FALSE,'business'),
  -- 成本核算（4）
  ('20000000-0000-4000-8000-00000000c141'::uuid,'business:erp:cost-period:calc',  3,'business:erp:cost-period:calc',  NULL,'20000000-0000-4000-8000-00000000c005'::uuid,NULL,10,1,NULL,'计算成本',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c142'::uuid,'business:erp:cost-period:submit',3,'business:erp:cost-period:submit',NULL,'20000000-0000-4000-8000-00000000c005'::uuid,NULL,20,1,NULL,'提交审批',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c143'::uuid,'business:erp:cost-period:settle',3,'business:erp:cost-period:settle',NULL,'20000000-0000-4000-8000-00000000c005'::uuid,NULL,30,1,NULL,'结算（入库+凭证）',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c144'::uuid,'business:erp:cost-period:reverse',3,'business:erp:cost-period:reverse',NULL,'20000000-0000-4000-8000-00000000c005'::uuid,NULL,40,1,NULL,'红冲结算',FALSE,'business'),
  -- 批次管理（2）
  ('20000000-0000-4000-8000-00000000c151'::uuid,'business:erp:lot:create',3,'business:erp:lot:create',NULL,'20000000-0000-4000-8000-00000000c006'::uuid,NULL,10,1,NULL,'新增批次',FALSE,'business'),
  ('20000000-0000-4000-8000-00000000c152'::uuid,'business:erp:lot:update',3,'business:erp:lot:update',NULL,'20000000-0000-4000-8000-00000000c006'::uuid,NULL,20,1,NULL,'修改批次',FALSE,'business')
ON CONFLICT (id) DO NOTHING;

-- ----------------------------------------------------------------------------
-- §3 角色授权（矩阵见文件头）
--     scope：FIN = 财务 5 页 + 17 个非红冲按钮；FIN_DIR = 红冲结算（只总监）；
--            WH  = 批次管理页 + 2 个按钮
--     角色不存在时（空库首启 SUPER_ADMIN/ADMIN 尚未种，客户可能删过角色）→ join 空转，
--     由 §4 自检打 NOTICE 说明，不阻断。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    fin_ids UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000c001','20000000-0000-4000-8000-00000000c002',
        '20000000-0000-4000-8000-00000000c003','20000000-0000-4000-8000-00000000c004',
        '20000000-0000-4000-8000-00000000c005',
        '20000000-0000-4000-8000-00000000c101','20000000-0000-4000-8000-00000000c102',
        '20000000-0000-4000-8000-00000000c103','20000000-0000-4000-8000-00000000c104',
        '20000000-0000-4000-8000-00000000c111','20000000-0000-4000-8000-00000000c112',
        '20000000-0000-4000-8000-00000000c113','20000000-0000-4000-8000-00000000c114',
        '20000000-0000-4000-8000-00000000c121','20000000-0000-4000-8000-00000000c122',
        '20000000-0000-4000-8000-00000000c131','20000000-0000-4000-8000-00000000c132',
        '20000000-0000-4000-8000-00000000c133','20000000-0000-4000-8000-00000000c134',
        '20000000-0000-4000-8000-00000000c141','20000000-0000-4000-8000-00000000c142',
        '20000000-0000-4000-8000-00000000c143'
    ]::uuid[];
    fin_dir_ids UUID[] := ARRAY['20000000-0000-4000-8000-00000000c144']::uuid[];
    wh_ids UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000c006',
        '20000000-0000-4000-8000-00000000c151',
        '20000000-0000-4000-8000-00000000c152'
    ]::uuid[];
    r RECORD;
    v_role_id UUID;
BEGIN
    -- 五条角色 → 应授的菜单集合（红冲结算 c144 只进结算总监那一行）
    FOR r IN
        SELECT * FROM (VALUES
            ('SUPER_ADMIN',      fin_ids || fin_dir_ids || wh_ids),
            ('ADMIN',            fin_ids || fin_dir_ids || wh_ids),
            ('FINANCE_MANAGER',  fin_ids),
            ('FINANCE_DIRECTOR', fin_ids || fin_dir_ids),
            ('WAREHOUSE_MANAGER', wh_ids)
        ) AS v(role_code, ids)
    LOOP
        BEGIN
            SELECT id INTO STRICT v_role_id FROM sys_role WHERE role_code = r.role_code AND deleted_at IS NULL;
            INSERT INTO sys_role_menu (role_id, menu_id)
            SELECT v_role_id, unnest(r.ids)
            ON CONFLICT (role_id, menu_id) DO NOTHING;
        EXCEPTION WHEN NO_DATA_FOUND THEN
            -- 角色不存在（空库首启 SUPER_ADMIN/ADMIN 由 seed 后建；客户也可能删过角色）
            RAISE NOTICE 'V0378：角色 % 不存在 → 跳过它的授权（自检另打提示）', r.role_code;
        END;
    END LOOP;
END $$;

-- ----------------------------------------------------------------------------
-- §4 自检（任一不符 → 迁移失败；宁可启动失败也不要「半套菜单 / 半套授权」上线）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    own_pages UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000c001','20000000-0000-4000-8000-00000000c002',
        '20000000-0000-4000-8000-00000000c003','20000000-0000-4000-8000-00000000c004',
        '20000000-0000-4000-8000-00000000c005','20000000-0000-4000-8000-00000000c006'
    ]::uuid[];
    own_btns UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000c101','20000000-0000-4000-8000-00000000c102',
        '20000000-0000-4000-8000-00000000c103','20000000-0000-4000-8000-00000000c104',
        '20000000-0000-4000-8000-00000000c111','20000000-0000-4000-8000-00000000c112',
        '20000000-0000-4000-8000-00000000c113','20000000-0000-4000-8000-00000000c114',
        '20000000-0000-4000-8000-00000000c121','20000000-0000-4000-8000-00000000c122',
        '20000000-0000-4000-8000-00000000c131','20000000-0000-4000-8000-00000000c132',
        '20000000-0000-4000-8000-00000000c133','20000000-0000-4000-8000-00000000c134',
        '20000000-0000-4000-8000-00000000c141','20000000-0000-4000-8000-00000000c142',
        '20000000-0000-4000-8000-00000000c143','20000000-0000-4000-8000-00000000c144',
        '20000000-0000-4000-8000-00000000c151','20000000-0000-4000-8000-00000000c152'
    ]::uuid[];
    n_pages       INT;
    n_btns        INT;
    n_bad_page    INT;
    n_bad_btn     INT;
    n_dup_sort    INT;
    n_long_title  INT;
    n_bad_icon    INT;
    n_orphan      INT;
    n_granted     INT;
    total_menus   INT;
    rc            TEXT;
    expect        INT;
BEGIN
    -- 4.1 6 页齐、型态对（父级 / sort / type / 可见 / 组件路径 / title≤12）
    SELECT count(*) INTO n_pages FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND id = ANY(own_pages);
    IF n_pages <> 6 THEN
        RAISE EXCEPTION 'V0378 自检失败：成本页面菜单应有 6 个（menu_type=2），实际 %', n_pages;
    END IF;

    SELECT count(*) INTO n_bad_page FROM (VALUES
        ('20000000-0000-4000-8000-00000000c001'::uuid, '/business/erp/cost-object',      '20000000-0000-0000-0000-000000000e02'::uuid, 60, '成本对象', 'layers',     'business/erp/cost-object/index'),
        ('20000000-0000-4000-8000-00000000c002'::uuid, '/business/erp/cost-labor',       '20000000-0000-0000-0000-000000000e02'::uuid, 62, '工时台账', 'time',       'business/erp/cost-labor/index'),
        ('20000000-0000-4000-8000-00000000c003'::uuid, '/business/erp/cost-overhead',    '20000000-0000-0000-0000-000000000e02'::uuid, 64, '费用台账', 'money',      'business/erp/cost-overhead/index'),
        ('20000000-0000-4000-8000-00000000c004'::uuid, '/business/erp/cost-alloc-rule',  '20000000-0000-0000-0000-000000000e02'::uuid, 66, '分摊规则', 'share',      'business/erp/cost-alloc-rule/index'),
        ('20000000-0000-4000-8000-00000000c005'::uuid, '/business/erp/cost-period',      '20000000-0000-0000-0000-000000000e02'::uuid, 68, '成本核算', 'calculator', 'business/erp/cost-period/index'),
        ('20000000-0000-4000-8000-00000000c006'::uuid, '/business/erp/lot',              '20000000-0000-0000-0000-000000000e01'::uuid, 95, '批次管理', 'barcode',    'business/erp/lot/index')
    ) AS v(id, path, parent_id, sort_order, title, icon, component)
    LEFT JOIN sys_menu m ON m.id = v.id AND m.deleted_at IS NULL
     WHERE m.id IS NULL
        OR m.path <> v.path OR m.parent_id <> v.parent_id OR m.sort_order <> v.sort_order
        OR m.title <> v.title OR m.icon <> v.icon OR m.component <> v.component
        OR m.is_visible IS NOT TRUE OR m.status <> 1;
    IF n_bad_page > 0 THEN
        RAISE EXCEPTION 'V0378 自检失败：% 个成本页面菜单的形态与设计 §6.1 不符（path/父级/sort/title/icon/component）', n_bad_page;
    END IF;

    -- 4.2 20 个按钮齐、挂钩正确、权限码与 name 一致
    SELECT count(*) INTO n_btns FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 3 AND id = ANY(own_btns);
    IF n_btns <> 20 THEN
        RAISE EXCEPTION 'V0378 自检失败：成本按钮权限应有 20 个（menu_type=3），实际 %', n_btns;
    END IF;

    SELECT count(*) INTO n_bad_btn
      FROM sys_menu b
     WHERE b.id = ANY(own_btns) AND b.deleted_at IS NULL
       AND (b.menu_type <> 3 OR b.is_visible IS NOT FALSE OR b.name <> b.auth_code
            OR b.auth_code NOT LIKE 'business:erp:%'
            OR b.parent_id <> CASE
                   WHEN b.id IN ('20000000-0000-4000-8000-00000000c101','20000000-0000-4000-8000-00000000c102','20000000-0000-4000-8000-00000000c103','20000000-0000-4000-8000-00000000c104') THEN '20000000-0000-4000-8000-00000000c001'::uuid
                   WHEN b.id IN ('20000000-0000-4000-8000-00000000c111','20000000-0000-4000-8000-00000000c112','20000000-0000-4000-8000-00000000c113','20000000-0000-4000-8000-00000000c114') THEN '20000000-0000-4000-8000-00000000c002'::uuid
                   WHEN b.id IN ('20000000-0000-4000-8000-00000000c121','20000000-0000-4000-8000-00000000c122') THEN '20000000-0000-4000-8000-00000000c003'::uuid
                   WHEN b.id IN ('20000000-0000-4000-8000-00000000c131','20000000-0000-4000-8000-00000000c132','20000000-0000-4000-8000-00000000c133','20000000-0000-4000-8000-00000000c134') THEN '20000000-0000-4000-8000-00000000c004'::uuid
                   WHEN b.id IN ('20000000-0000-4000-8000-00000000c141','20000000-0000-4000-8000-00000000c142','20000000-0000-4000-8000-00000000c143','20000000-0000-4000-8000-00000000c144') THEN '20000000-0000-4000-8000-00000000c005'::uuid
                   ELSE '20000000-0000-4000-8000-00000000c006'::uuid
               END);
    IF n_bad_btn > 0 THEN
        RAISE EXCEPTION 'V0378 自检失败：% 个成本按钮权限点的形态不对（menu_type/is_visible/name=auth_code/父页面）', n_bad_btn;
    END IF;

    -- 4.3 26 个权限码在全库唯一（AGENTS 约定 7：auth_code 与 sys_menu 一一对应）
    --     口径：本块 26 个 id 之外，不许有别的活菜单持有同一 auth_code（自建同码页面会让
    --     「按 auth_code 反查菜单」的授权树画出两条分支）
    SELECT count(*) INTO n_dup_sort FROM sys_menu
     WHERE deleted_at IS NULL
       AND auth_code IN (SELECT auth_code FROM sys_menu WHERE id = ANY(own_pages || own_btns))
       AND NOT (id = ANY(own_pages || own_btns));
    IF n_dup_sort > 0 THEN
        RAISE EXCEPTION 'V0378 自检失败：% 条 auth_code 被本迁移之外的菜单占用（授权树会分叉）', n_dup_sort;
    END IF;

    -- 4.4 同父 sort 唯一（只查本迁移碰的两个父节点；全库口径由 menu_invariants.sh ⑦ 兜）
    SELECT count(*) INTO n_dup_sort FROM (
        SELECT parent_id, sort_order FROM sys_menu
         WHERE deleted_at IS NULL AND menu_type = 2
           AND parent_id IN ('20000000-0000-0000-0000-000000000e01'::uuid,
                             '20000000-0000-0000-0000-000000000e02'::uuid)
         GROUP BY parent_id, sort_order HAVING count(*) > 1
    ) d;
    IF n_dup_sort > 0 THEN
        RAISE EXCEPTION 'V0378 自检失败：财务管理/仓库管理下 % 组 sort_order 撞车（侧栏顺序会漂）', n_dup_sort;
    END IF;

    -- 4.5 title 长度（规范 1.6.1：侧栏显示名 ≤12 字，目标 2-6 字）
    SELECT count(*) INTO n_long_title FROM sys_menu
     WHERE deleted_at IS NULL AND id = ANY(own_pages) AND length(title) > 12;
    IF n_long_title > 0 THEN
        RAISE EXCEPTION 'V0378 自检失败：% 个成本页面 title 超过 12 字', n_long_title;
    END IF;

    -- 4.6 icon 非空（真实存在性由 menu_invariants.sh ⑧ 比对图标库）
    SELECT count(*) INTO n_bad_icon FROM sys_menu
     WHERE deleted_at IS NULL AND id = ANY(own_pages) AND (icon IS NULL OR btrim(icon) = '');
    IF n_bad_icon > 0 THEN
        RAISE EXCEPTION 'V0378 自检失败：% 个成本页面没设 icon（侧栏会空图标）', n_bad_icon;
    END IF;

    -- 4.7 授权：存在的角色必须拿到应有的条数，且**无孤儿**（授了按钮必须授父页）
    FOREACH rc IN ARRAY ARRAY['SUPER_ADMIN','ADMIN','FINANCE_MANAGER','FINANCE_DIRECTOR','WAREHOUSE_MANAGER']
    LOOP
        IF NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = rc AND deleted_at IS NULL) THEN
            RAISE NOTICE 'V0378：角色 % 不存在（空库首启 SUPER_ADMIN/ADMIN 由 seed 后建）→ 本迁移跳过它的授权', rc;
            CONTINUE;
        END IF;

        expect := CASE rc
                    WHEN 'FINANCE_MANAGER'  THEN 22   -- 5 页 + 17 个非红冲按钮
                    WHEN 'FINANCE_DIRECTOR' THEN 23   -- 5 页 + 17 按钮 + 红冲结算
                    WHEN 'WAREHOUSE_MANAGER' THEN 3   -- 1 页 + 2 按钮
                    ELSE 26                            -- 超管 / ADMIN：全部
                  END;
        SELECT count(*) INTO n_granted
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id
         WHERE r.role_code = rc AND r.deleted_at IS NULL
           AND rm.menu_id = ANY(own_pages || own_btns);
        IF n_granted <> expect THEN
            RAISE EXCEPTION 'V0378 自检失败：角色 % 应持有 % 条成本菜单授权，实际 %', rc, expect, n_granted;
        END IF;

        SELECT count(*) INTO n_orphan
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id
          JOIN sys_menu b ON b.id = rm.menu_id
         WHERE r.role_code = rc AND r.deleted_at IS NULL
           AND b.id = ANY(own_btns)
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm2
                            WHERE rm2.role_id = rm.role_id AND rm2.menu_id = b.parent_id);
        IF n_orphan > 0 THEN
            RAISE EXCEPTION 'V0378 自检失败：角色 % 有 % 个成本按钮是孤儿（父页面未授，进不去列表）', rc, n_orphan;
        END IF;
    END LOOP;

    -- 4.8 菜单总数（menu_invariants ⑤ 的上限 460；超了 gating 会红，先在迁移里点名）
    SELECT count(*) INTO total_menus FROM sys_menu WHERE deleted_at IS NULL;
    IF total_menus > 460 THEN
        RAISE EXCEPTION 'V0378 自检失败：菜单总数 % 超过 menu_invariants ⑤ 的上限 460（先评估再调阈值）', total_menus;
    END IF;
    RAISE NOTICE 'V0378：成本块 6 页 + 20 按钮已就绪，菜单总数 % 条（上限 460）', total_menus;
END $$;
