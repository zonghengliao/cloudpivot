-- ============================================================================
-- V0387：人力资源 · 菜单 + 权限点 + HR_MANAGER 角色 + 角色授权
--        （ARCH-9-2 人力资源设计 §3.2 ④⑤ / §5.2 / §6.1 / §8.1 HR-04）
--
-- 卡：t_248d6edc（HR-04【HR】迁移 V0381）
-- 依据：docs/architecture/ARCH-9-人力资源模块设计.md
--       §3.2 ④（菜单/权限：1 目录 + 5 页面 + 30 按钮 = 36 行）、
--       §3.2 ⑤（sys_role：HR_MANAGER + 授权范围）、
--       §5.2（46 条 API 的权限码）、§6.1（页面清单 / 挂载点 / icon）、§7.1（迁移清单）。
--       36 行的"完整 INSERT"出处是设计阶段真库证据脚本
--       docs/evidence/2026-11-10-arch9-2-hr-ddl/02_alter_tables_check.sql §④。
--
-- # 号段为什么是 V0387，而不是卡面的 V0381（第三次顺延，前两次见 §7.1）
--   V0378~V0382 已被成本核算实占（cost_menu_and_perm / cost_seq_and_dict / cost_lot_backfill /
--   cost_flow_def / stock_in_production_sourceless），V0383=hr_tables（HR-01）、
--   V0384=hr_accounts（HR-02）、V0385=cost_module_switch（COST-B4-4）、V0386=hr_seq_dict（HR-03）。
--   本仓迁移加载器对版本号做**跨目录全局唯一**校验（backend/vendor/nik-db/src/migrate/loader.rs），
--   重号 = 服务启动直接失败（AGENTS 约定 6）。⇒ HR-04 落 **V0387**（设计 §7.1 表"实际号"列）。
--
-- # ★ 相对证据脚本的两处 UUID 纠偏（真库实测，不是"看着像冲突"）
--   设计阶段证据脚本（2026-11-10）用的两段 UUID 在**今天的库里已被占用**：
--     ① 菜单 `20000000-0000-0000-0000-000000001600` —— 已是
--        `business:erp:report:payable-aging`（应付账龄分析，/business/erp/report/payable-aging）。
--        36 行里 `…001600~001653` 与它同族，直接照抄会让首行 ON CONFLICT 静默不插、
--        后续 35 行插进去 → 36 行少 1 行且挂在别人 id 上，自检才会红。
--     ② 角色 `20000000-0000-4000-8000-0000000f0008` —— 已是 **SALES_CLERK**（销售员）。
--        照抄会让 HR_MANAGER 的 INSERT 被 `NOT EXISTS (role_code='HR_MANAGER')` 放行、
--        却用同一个 id 撞 `sys_role` 主键 → 迁移报 duplicate key。
--   ⇒ 本迁移改用**空闲**号段（两个库实测均 0 行占用）：
--     菜单 `20000000-0000-4000-8000-00000000e0xx`（与成本卡的 c0xx 同族但不同段）
--     角色 `20000000-0000-4000-8000-0000000f0016`（f0001~f0009、f0015 已占，f0016 空闲）
--   末三位沿用证据脚本的语义分组（e000 目录 / e001-e005 页面 / e011-e019 员工 /
--   e021-e025 考勤 / e031-e036 请假 / e041-e047 薪资 / e051-e053 薪资项）。
--
-- # 挂载点（真库实测，不是猜的）
--   顶级目录 `parent_id IS NULL`，`sort_order = 77`：145/99 新库与 144 生产**均空闲**
--   （实测生产顶级：5/10/18/20/55/60/62/64/65/66/70/72/75/75/76/78/95）。
--   页面挂本目录下 sort 10/20/30/40/50，按钮挂对应页面 sort 10 递增（AGENTS 约定 12：
--   同父 sort 唯一；⑨ 组断言在 §4 自检与 scripts/verify/menu_invariants.sh ⑦ 各兜一道）。
--
-- # 菜单口径（AGENTS 约定 12）
--   name  = 页面用中文短名（与 title 一致，与成本/ERP 页面行同款）；按钮用权限码本身
--   title = 侧边栏显示名，2-6 字，不带标点/说明/章节号（本迁移 6 个 1/2 级全部 2-4 字）
--   icon  = TDesign 真实图标名，本卡 6 个逐个 `ls frontend/node_modules/tdesign-icons-vue-next/
--           esm/components/<n>.js` 实证存在（user-business / user / calendar / time / wallet / setting）
--   module_code = 'business'；目录 component = 'Layout'（与 /system、/support 目录同款）
--
-- # 与前端的关系（先种菜单、后接页面，是刻意排序）
--   页面指向的组件（business/hr/*）由 HR-18/28/29/43/44 落地。菜单先种不影响运行：
--   路由未注册时点进去是 404，前端卡接上即通；反过来"页面先有、菜单后在"会让角色授权树
--   缺分支、`menu_invariants` 与授权自检都不敢跑。这是成本卡 B1 批次的既有打法。
--   ⚠ 连带后果：30 个权限码此刻**后端还没有 `#[nik::permission]` 落地**
--   （handler 在 HR-12/25/31/39），`system::menu::permission_audit_tests` 的 N14 会判"死码"。
--   故同一提交在 permission_audit_tests.rs 增 `PENDING_LANDING` 临时台账（每条写明由哪张卡落地；
--   一旦后端出现引用，N18 会判台账过期 → 强制删除）。这是"菜单先种"唯一诚实的处理方式，
--   不留红、也不靠 SKIP。
--
-- # 角色授权矩阵（设计 §3.2 ⑤ 只给了范围，逐码由本卡定稿；两条硬约束）
--   ① **无孤儿**（V0258 §五 + S__employee_self_service_permissions 的实测教训）：
--      授按钮码必授其父页面；授页面必授**祖先目录**（人力资源 e000）—— 非超管缺祖先闭包时
--      侧边栏整棵子树被静默丢掉，功能"存在但看不见"；
--   ② **不越权**：红冲（salary:reverse）只给 HR_MANAGER（人事经理出工资单并负责红冲），
--      财务经理只给"看 + 过账 + 导出"，不给红冲 —— 与 §4.6 工资凭证"过账后不可改、红冲留痕"同口径。
--
--     SUPER_ADMIN    36（全部；超管本可短路校验，显式授予是为了「角色-菜单」树上能看清范围）
--     HR_MANAGER     36（人事经理：档案 / 考勤 / 请假 / 薪资全流程）
--     FINANCE_MANAGER 5（目录 + 薪资管理页 + salary:{detail,post,export}）
--     DEPT_LEADER     5（目录 + 员工档案页 + employee:detail + 请假单页 + leave:detail）
--     EMPLOYEE        5（目录 + 请假单页 + leave:create + 薪资管理页 + salary:my）
--                    —— 自助口径（§6.1 "员工自助不新增菜单，以页面内 tab 提供"）：只给页面
--                       外壳 + 两个自助码，页面里的列表接口要 salary:list / leave:list，员工
--                       没有 → 只显示"我的薪资 / 我的请假"tab（前端按权限码显隐，§6.1 表）。
--
--   ⚠ 已知缺口（报出、本卡不改）：§6.1 的"我的档案"tab 要 `GET /employee/{my_id}`，
--     该接口的权限码是 `business:hr:employee:detail`，而 EMPLOYEE 按设计只拿 2 个码 →
--     员工看不到自己的档案。这属于 HR-47（权限矩阵回归 · 30 码 × 6 角色）的决策面，
--     本卡不擅自给 employee:detail（会把"看全员档案"的码发给全员，越权）。
--
-- # 幂等
--   36 条菜单 = 固定 UUID + `ON CONFLICT (id) DO NOTHING`（不覆盖人工改名）；
--   角色 = `NOT EXISTS (role_code='HR_MANAGER')`（sys_role **没有** role_code 唯一约束，
--          只能靠判存在，重号靠主键兜）；授权 = `ON CONFLICT (role_id, menu_id) DO NOTHING`
--          （sys_role_menu 有 UNIQUE(role_id, menu_id)）。重放 = 0 行插入、0 行更新。
--
-- # 回滚
--   V0387.down.sql：删授权（只按本迁移的 36 个 menu_id，不按 role_code 通配）→ 删 30 按钮
--   → 删 5 页面 → 删目录 → （有条件地）删 HR_MANAGER。三道护栏：页面下有本迁移之外的子菜单 /
--   按钮下有任何子节点 / 角色已被授了本迁移之外的菜单或绑了用户 → 前两条拒绝执行，第三条
--   只保留角色、不删（不吞别人的数据）。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- §0 前置护栏：6 个 path / 36 个 auth_code 不许被**别的 id** 占着
--     只靠 `ON CONFLICT (id) DO NOTHING` 有个洞：客户若自建过 `/business/hr/employee`
--     （不同 id），我们会再插一条同 path 的菜单，menu_invariants ④（path 唯一）立刻红，
--     而红的位置在迁移之外，排查成本高。这里提前点名并停住。
--     （本段只读不写：命中即 RAISE，库不留半套数据。）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    own_ids UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e000',
        '20000000-0000-4000-8000-00000000e001','20000000-0000-4000-8000-00000000e002',
        '20000000-0000-4000-8000-00000000e003','20000000-0000-4000-8000-00000000e004',
        '20000000-0000-4000-8000-00000000e005',
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
    own_paths TEXT[] := ARRAY[
        '/business/hr','/business/hr/employee','/business/hr/attendance',
        '/business/hr/leave','/business/hr/salary','/business/hr/salary-item'
    ];
    n_conflict INT;
    sample     TEXT;
BEGIN
    -- 0.1 path 被别人占（含软删记录：软删也占着 path，恢复时会撞）
    SELECT count(*), string_agg(path || ' → ' || id::text, '; ')
      INTO n_conflict, sample
      FROM sys_menu
     WHERE path = ANY(own_paths) AND NOT (id = ANY(own_ids));
    IF n_conflict > 0 THEN
        RAISE EXCEPTION 'V0387 拒绝执行：% 条人力资源页面 path 已被其它菜单占用（%）。请先处理同名页面再升级',
            n_conflict, sample;
    END IF;

    -- 0.2 权限码被别人占（AGENTS 约定 7：auth_code 与 sys_menu 一一对应）
    SELECT count(*), string_agg(m.auth_code || ' → ' || m.id::text, '; ')
      INTO n_conflict, sample
      FROM sys_menu m
     WHERE m.auth_code LIKE 'business:hr:%'
       AND NOT (m.id = ANY(own_ids));
    IF n_conflict > 0 THEN
        RAISE EXCEPTION 'V0387 拒绝执行：% 条 business:hr:* 权限码已被其它菜单占用（%）。请先处理同码权限点再升级',
            n_conflict, sample;
    END IF;

    -- 0.3 顶级排序位 77 被占（页面会与别人的目录并排/漂序）
    SELECT count(*), string_agg(name || ' (' || id::text || ')', '; ')
      INTO n_conflict, sample
      FROM sys_menu
     WHERE deleted_at IS NULL AND parent_id IS NULL AND sort_order = 77
       AND NOT (id = ANY(own_ids));
    IF n_conflict > 0 THEN
        RAISE EXCEPTION 'V0387 拒绝执行：顶级 sort_order=77 已被 % 占用（%）—— 人力资源目录无法按设计落位，请先腾出该排序位',
            n_conflict, sample;
    END IF;

    -- 0.4 角色码已被别的 id 占用 → 不阻断（授权会授给它），但要留痕，
    --     因为 down 只会删"本迁移 id 的那个角色"
    IF EXISTS (SELECT 1 FROM sys_role
                WHERE role_code = 'HR_MANAGER'
                  AND id <> '20000000-0000-4000-8000-0000000f0016'::uuid) THEN
        RAISE NOTICE 'V0387：已存在其它 id 的 HR_MANAGER 角色 → 复用该角色授权，本迁移不再新建（down 也不会删它）';
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- §1 目录 + 页面（menu_type 1/2，进侧栏）
--     顺序即数据流：先有档案 → 才能排考勤 → 才谈请假 → 才算薪 → 才配薪资项
--     （薪资项排最后：它是配置项，日常点击频次最低）
-- ----------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
  -- 目录（顶级）
  ('20000000-0000-4000-8000-00000000e000'::uuid, '人力资源', 1, '',
   '/business/hr', NULL, 'Layout', 77, 1, 'user-business', '人力资源', TRUE, 'business'),
  -- 5 个页面
  ('20000000-0000-4000-8000-00000000e001'::uuid, '员工档案', 2, 'business:hr:employee:list',
   '/business/hr/employee', '20000000-0000-4000-8000-00000000e000'::uuid,
   'business/hr/employee/index', 10, 1, 'user', '员工档案', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000e002'::uuid, '考勤管理', 2, 'business:hr:attendance:list',
   '/business/hr/attendance', '20000000-0000-4000-8000-00000000e000'::uuid,
   'business/hr/attendance/index', 20, 1, 'calendar', '考勤管理', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000e003'::uuid, '请假单', 2, 'business:hr:leave:list',
   '/business/hr/leave', '20000000-0000-4000-8000-00000000e000'::uuid,
   'business/hr/leave/index', 30, 1, 'time', '请假单', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000e004'::uuid, '薪资管理', 2, 'business:hr:salary:list',
   '/business/hr/salary', '20000000-0000-4000-8000-00000000e000'::uuid,
   'business/hr/salary/index', 40, 1, 'wallet', '薪资管理', TRUE, 'business'),
  ('20000000-0000-4000-8000-00000000e005'::uuid, '薪资项', 2, 'business:hr:salary-item:list',
   '/business/hr/salary-item', '20000000-0000-4000-8000-00000000e000'::uuid,
   'business/hr/salary-item/index', 50, 1, 'setting', '薪资项', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ----------------------------------------------------------------------------
-- §2 按钮权限点（menu_type = 3，is_visible = FALSE，不进侧栏）
--     30 个 = 设计 §3.2 ④ 的 30 行，逐条对照 §5.2 的权限码清单（46 条 API 去重后 30 码）：
--       员工档案 9（含 §5.2 #1~#15 的 employee:{list,detail,create,update,delete,sensitive,
--                  import,export} + contract:manage；employee:resign 是离职状态流转，
--                  §5.2 #6 用 employee:update 的粒度太粗，设计 §3.2 ④ 单列一码，本迁移照设计）
--       考勤 5（attendance:{period,record,import,export,lock}；#25 的 /my 登录即可、不设码）
--       请假 6（leave:{create,update,delete,submit,cancel,detail}）
--       薪资 7（salary:{detail,calc,submit,post,reverse,export,my}）
--       薪资项 3（salary-item:{create,update,delete}；#35 validate 复用 update 码，与设计一致）
-- ----------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
  -- 员工档案（9）：父 = e001
  ('20000000-0000-4000-8000-00000000e011'::uuid,'business:hr:employee:create',   3,'business:hr:employee:create',   NULL,'20000000-0000-4000-8000-00000000e001'::uuid,NULL,10,1,NULL,'新建员工',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e012'::uuid,'business:hr:employee:update',   3,'business:hr:employee:update',   NULL,'20000000-0000-4000-8000-00000000e001'::uuid,NULL,20,1,NULL,'编辑员工',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e013'::uuid,'business:hr:employee:delete',   3,'business:hr:employee:delete',   NULL,'20000000-0000-4000-8000-00000000e001'::uuid,NULL,30,1,NULL,'删除员工',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e014'::uuid,'business:hr:employee:detail',   3,'business:hr:employee:detail',   NULL,'20000000-0000-4000-8000-00000000e001'::uuid,NULL,40,1,NULL,'员工详情',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e015'::uuid,'business:hr:employee:sensitive',3,'business:hr:employee:sensitive',NULL,'20000000-0000-4000-8000-00000000e001'::uuid,NULL,50,1,NULL,'查看明文（身份证/银行账号）',    FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e016'::uuid,'business:hr:employee:resign',   3,'business:hr:employee:resign',   NULL,'20000000-0000-4000-8000-00000000e001'::uuid,NULL,60,1,NULL,'发起离职',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e017'::uuid,'business:hr:employee:import',   3,'business:hr:employee:import',   NULL,'20000000-0000-4000-8000-00000000e001'::uuid,NULL,70,1,NULL,'导入员工',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e018'::uuid,'business:hr:employee:export',   3,'business:hr:employee:export',   NULL,'20000000-0000-4000-8000-00000000e001'::uuid,NULL,80,1,NULL,'导出员工',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e019'::uuid,'business:hr:contract:manage',   3,'business:hr:contract:manage',   NULL,'20000000-0000-4000-8000-00000000e001'::uuid,NULL,90,1,NULL,'合同台账维护',                  FALSE,'business'),
  -- 考勤（5）：父 = e002
  ('20000000-0000-4000-8000-00000000e021'::uuid,'business:hr:attendance:period', 3,'business:hr:attendance:period', NULL,'20000000-0000-4000-8000-00000000e002'::uuid,NULL,10,1,NULL,'考勤期间维护',                  FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e022'::uuid,'business:hr:attendance:record', 3,'business:hr:attendance:record', NULL,'20000000-0000-4000-8000-00000000e002'::uuid,NULL,20,1,NULL,'考勤记录维护',                  FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e023'::uuid,'business:hr:attendance:import', 3,'business:hr:attendance:import', NULL,'20000000-0000-4000-8000-00000000e002'::uuid,NULL,30,1,NULL,'导入考勤',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e024'::uuid,'business:hr:attendance:export', 3,'business:hr:attendance:export', NULL,'20000000-0000-4000-8000-00000000e002'::uuid,NULL,40,1,NULL,'导出考勤',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e025'::uuid,'business:hr:attendance:lock',   3,'business:hr:attendance:lock',   NULL,'20000000-0000-4000-8000-00000000e002'::uuid,NULL,50,1,NULL,'锁定/解锁考勤期间',             FALSE,'business'),
  -- 请假（6）：父 = e003
  ('20000000-0000-4000-8000-00000000e031'::uuid,'business:hr:leave:create',      3,'business:hr:leave:create',      NULL,'20000000-0000-4000-8000-00000000e003'::uuid,NULL,10,1,NULL,'新建请假单',                    FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e032'::uuid,'business:hr:leave:update',      3,'business:hr:leave:update',      NULL,'20000000-0000-4000-8000-00000000e003'::uuid,NULL,20,1,NULL,'编辑请假单',                    FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e033'::uuid,'business:hr:leave:delete',      3,'business:hr:leave:delete',      NULL,'20000000-0000-4000-8000-00000000e003'::uuid,NULL,30,1,NULL,'删除请假单',                    FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e034'::uuid,'business:hr:leave:submit',      3,'business:hr:leave:submit',      NULL,'20000000-0000-4000-8000-00000000e003'::uuid,NULL,40,1,NULL,'提交审批',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e035'::uuid,'business:hr:leave:cancel',      3,'business:hr:leave:cancel',      NULL,'20000000-0000-4000-8000-00000000e003'::uuid,NULL,50,1,NULL,'作废请假单',                    FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e036'::uuid,'business:hr:leave:detail',      3,'business:hr:leave:detail',      NULL,'20000000-0000-4000-8000-00000000e003'::uuid,NULL,60,1,NULL,'请假单详情',                    FALSE,'business'),
  -- 薪资单（7）：父 = e004
  ('20000000-0000-4000-8000-00000000e041'::uuid,'business:hr:salary:detail',     3,'business:hr:salary:detail',     NULL,'20000000-0000-4000-8000-00000000e004'::uuid,NULL,10,1,NULL,'薪资单详情',                    FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e042'::uuid,'business:hr:salary:calc',       3,'business:hr:salary:calc',       NULL,'20000000-0000-4000-8000-00000000e004'::uuid,NULL,20,1,NULL,'试算/重算',                     FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e043'::uuid,'business:hr:salary:submit',     3,'business:hr:salary:submit',     NULL,'20000000-0000-4000-8000-00000000e004'::uuid,NULL,30,1,NULL,'提交审批',                      FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e044'::uuid,'business:hr:salary:post',       3,'business:hr:salary:post',       NULL,'20000000-0000-4000-8000-00000000e004'::uuid,NULL,40,1,NULL,'过账（生成凭证）',              FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e045'::uuid,'business:hr:salary:reverse',    3,'business:hr:salary:reverse',    NULL,'20000000-0000-4000-8000-00000000e004'::uuid,NULL,50,1,NULL,'红冲',                          FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e046'::uuid,'business:hr:salary:export',     3,'business:hr:salary:export',     NULL,'20000000-0000-4000-8000-00000000e004'::uuid,NULL,60,1,NULL,'导出工资表',                    FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e047'::uuid,'business:hr:salary:my',         3,'business:hr:salary:my',         NULL,'20000000-0000-4000-8000-00000000e004'::uuid,NULL,70,1,NULL,'我的薪资（自助）',              FALSE,'business'),
  -- 薪资项（3）：父 = e005
  ('20000000-0000-4000-8000-00000000e051'::uuid,'business:hr:salary-item:create',3,'business:hr:salary-item:create',NULL,'20000000-0000-4000-8000-00000000e005'::uuid,NULL,10,1,NULL,'新建薪资项',                    FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e052'::uuid,'business:hr:salary-item:update',3,'business:hr:salary-item:update',NULL,'20000000-0000-4000-8000-00000000e005'::uuid,NULL,20,1,NULL,'编辑薪资项',                    FALSE,'business'),
  ('20000000-0000-4000-8000-00000000e053'::uuid,'business:hr:salary-item:delete',3,'business:hr:salary-item:delete',NULL,'20000000-0000-4000-8000-00000000e005'::uuid,NULL,30,1,NULL,'删除薪资项',                    FALSE,'business')
ON CONFLICT (id) DO NOTHING;

-- ----------------------------------------------------------------------------
-- §3 角色 HR_MANAGER（人事经理，设计 §3.2 ⑤）
--     data_scope = 3（本部门及以下）：人事经理默认看自己部门子树，跨部门看全公司要超管或改范围。
--     sort_order = 25（设计写 80，但 80 已被 SALES_CLERK 占用；25 落在财务角色 20/21 之后、
--     销售员 80 之前，语义顺）。sys_role 的 role_code 无唯一约束 ⇒ 幂等靠 NOT EXISTS。
-- ----------------------------------------------------------------------------
INSERT INTO sys_role (id, role_code, role_name, description, status, data_scope, sort_order)
SELECT '20000000-0000-4000-8000-0000000f0016'::uuid, 'HR_MANAGER', '人事经理',
       'ARCH-9-2：员工档案 / 考勤 / 薪资维护；无财务过账权（过账另有 salary:post 码）',
       1, 3, 25
 WHERE NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL);

-- ----------------------------------------------------------------------------
-- §4 角色授权
--     逐角色写一段（而不是一张 VALUES 矩阵）：每段角色码是显式 `role_code = 'X'` 字面量，
--     能被 scripts/verify/role_code_gate.py 扫到（矩阵写法它扫不到 = 假绿）；
--     角色不存在（客户删过 / 空库首启顺序变化）→ NOTICE 跳过，不阻断，自检另打提示。
--     菜单集合一律含目录 e000（祖先闭包，见文件头硬约束 ①）。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_role UUID;
    n      INT;
    -- 全量 36（目录 + 5 页 + 30 按钮）
    all_hr UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e000',
        '20000000-0000-4000-8000-00000000e001','20000000-0000-4000-8000-00000000e002',
        '20000000-0000-4000-8000-00000000e003','20000000-0000-4000-8000-00000000e004',
        '20000000-0000-4000-8000-00000000e005',
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
    -- 财务经理 5：目录 + 薪资管理页 + 看/过账/导出（**不含红冲**，见文件头硬约束 ②）
    fin_hr UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e000',
        '20000000-0000-4000-8000-00000000e004',
        '20000000-0000-4000-8000-00000000e041',
        '20000000-0000-4000-8000-00000000e044',
        '20000000-0000-4000-8000-00000000e046'
    ]::uuid[];
    -- 部门主管 5：目录 + 员工档案页 + 员工详情 + 请假单页 + 请假详情（审批在流程中心，用 flow 码）
    lead_hr UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e000',
        '20000000-0000-4000-8000-00000000e001',
        '20000000-0000-4000-8000-00000000e014',
        '20000000-0000-4000-8000-00000000e003',
        '20000000-0000-4000-8000-00000000e036'
    ]::uuid[];
    -- 员工自助 5：目录 + 请假单页 + leave:create + 薪资管理页 + salary:my（见文件头）
    emp_hr UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e000',
        '20000000-0000-4000-8000-00000000e003',
        '20000000-0000-4000-8000-00000000e031',
        '20000000-0000-4000-8000-00000000e004',
        '20000000-0000-4000-8000-00000000e047'
    ]::uuid[];
BEGIN
    ------------------------------------------------------------------ SUPER_ADMIN
    SELECT id INTO v_role FROM sys_role WHERE role_code = 'SUPER_ADMIN' AND deleted_at IS NULL LIMIT 1;
    IF v_role IS NULL THEN
        RAISE NOTICE 'V0387：角色 SUPER_ADMIN 不存在 → 跳过它的授权（超管本可短路校验）';
    ELSE
        INSERT INTO sys_role_menu (role_id, menu_id)
        SELECT v_role, unnest(all_hr) ON CONFLICT (role_id, menu_id) DO NOTHING;
        GET DIAGNOSTICS n = ROW_COUNT;
        RAISE NOTICE 'V0387：SUPER_ADMIN 新增 % 条人力资源菜单授权', n;
    END IF;

    ------------------------------------------------------------------ HR_MANAGER
    SELECT id INTO v_role FROM sys_role WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL LIMIT 1;
    IF v_role IS NULL THEN
        RAISE EXCEPTION 'V0387 自检失败：HR_MANAGER 角色未建成（§3 应已插入）';
    ELSE
        INSERT INTO sys_role_menu (role_id, menu_id)
        SELECT v_role, unnest(all_hr) ON CONFLICT (role_id, menu_id) DO NOTHING;
        GET DIAGNOSTICS n = ROW_COUNT;
        RAISE NOTICE 'V0387：HR_MANAGER 新增 % 条人力资源菜单授权（应覆盖 36 条）', n;
    END IF;

    ------------------------------------------------------------------ FINANCE_MANAGER
    SELECT id INTO v_role FROM sys_role WHERE role_code = 'FINANCE_MANAGER' AND deleted_at IS NULL LIMIT 1;
    IF v_role IS NULL THEN
        RAISE NOTICE 'V0387：角色 FINANCE_MANAGER 不存在 → 跳过它的授权';
    ELSE
        INSERT INTO sys_role_menu (role_id, menu_id)
        SELECT v_role, unnest(fin_hr) ON CONFLICT (role_id, menu_id) DO NOTHING;
    END IF;

    ------------------------------------------------------------------ DEPT_LEADER
    SELECT id INTO v_role FROM sys_role WHERE role_code = 'DEPT_LEADER' AND deleted_at IS NULL LIMIT 1;
    IF v_role IS NULL THEN
        RAISE NOTICE 'V0387：角色 DEPT_LEADER 不存在 → 跳过它的授权';
    ELSE
        INSERT INTO sys_role_menu (role_id, menu_id)
        SELECT v_role, unnest(lead_hr) ON CONFLICT (role_id, menu_id) DO NOTHING;
    END IF;

    ------------------------------------------------------------------ EMPLOYEE（自助）
    SELECT id INTO v_role FROM sys_role WHERE role_code = 'EMPLOYEE' AND deleted_at IS NULL LIMIT 1;
    IF v_role IS NULL THEN
        RAISE NOTICE 'V0387：角色 EMPLOYEE 不存在 → 跳过它的授权（自助码由 S__employee_self_service_permissions 兜）';
    ELSE
        INSERT INTO sys_role_menu (role_id, menu_id)
        SELECT v_role, unnest(emp_hr) ON CONFLICT (role_id, menu_id) DO NOTHING;
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- §5 自检（任一不符 → 迁移失败；宁可启动失败也不要「半套菜单 / 半套授权」上线）
--     ⚠ 菜单总数上限 520 —— 与 scripts/verify/menu_invariants.sh ⑤ 的 MENU_TOTAL_MAX 同值。
--       调整史：400-420 → 400-460（2026-11-08，采购三个闭环 +13）→ 400-520（本卡：
--       成本 26 + 人力资源 36 两个模块同批落地，实测 432+26(库内已含)=459 → +36 = 495，
--       留 ~5% 余量）。上限是「防批量误插/误删」的护栏，不是功能增长闸门。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    own_dir    UUID := '20000000-0000-4000-8000-00000000e000'::uuid;
    own_pages  UUID[] := ARRAY[
        '20000000-0000-4000-8000-00000000e001','20000000-0000-4000-8000-00000000e002',
        '20000000-0000-4000-8000-00000000e003','20000000-0000-4000-8000-00000000e004',
        '20000000-0000-4000-8000-00000000e005'
    ]::uuid[];
    own_btns   UUID[] := ARRAY[
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
    all_ids    UUID[];
    n_rows     INT;
    n_bad      INT;
    n_dup      INT;
    n_grant    INT;
    n_orphan   INT;
    n_total    INT;
    rc         TEXT;
    expect     INT;
    sample     TEXT;
BEGIN
    all_ids := ARRAY[own_dir] || own_pages || own_btns;

    -- 5.1 36 行齐（1 目录 + 5 页面 + 30 按钮），且只此 36 行
    SELECT count(*) INTO n_rows FROM sys_menu WHERE deleted_at IS NULL AND id = ANY(all_ids);
    IF n_rows <> 36 THEN
        RAISE EXCEPTION 'V0387 自检失败：人力资源菜单应有 36 行（1 目录 + 5 页面 + 30 按钮），实际 %', n_rows;
    END IF;

    -- 5.2 目录形态（顶级 / sort 77 / type 1 / 可见 / 组件 Layout / icon / title ≤12）
    SELECT count(*) INTO n_bad
      FROM sys_menu m
     WHERE m.id = own_dir
       AND NOT (m.deleted_at IS NULL AND m.menu_type = 1 AND m.parent_id IS NULL
                AND m.sort_order = 77 AND m.is_visible IS TRUE AND m.status = 1
                AND m.path = '/business/hr' AND m.title = '人力资源'
                AND m.component = 'Layout' AND m.icon = 'user-business'
                AND coalesce(m.module_code, 'business') = 'business');
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0387 自检失败：人力资源目录形态与设计 §6.1 不符（顶级/sort 77/type 1/可见/Layout/user-business/人力资源）';
    END IF;

    -- 5.3 5 个页面形态（父=目录 / path / sort / component / title / icon / 可见）
    SELECT count(*) INTO n_bad FROM (VALUES
        ('20000000-0000-4000-8000-00000000e001'::uuid, '/business/hr/employee',    10, '员工档案', 'user',     'business/hr/employee/index'),
        ('20000000-0000-4000-8000-00000000e002'::uuid, '/business/hr/attendance',  20, '考勤管理', 'calendar', 'business/hr/attendance/index'),
        ('20000000-0000-4000-8000-00000000e003'::uuid, '/business/hr/leave',       30, '请假单',   'time',     'business/hr/leave/index'),
        ('20000000-0000-4000-8000-00000000e004'::uuid, '/business/hr/salary',      40, '薪资管理', 'wallet',   'business/hr/salary/index'),
        ('20000000-0000-4000-8000-00000000e005'::uuid, '/business/hr/salary-item', 50, '薪资项',   'setting',  'business/hr/salary-item/index')
    ) AS v(id, path, sort_order, title, icon, component)
    LEFT JOIN sys_menu m ON m.id = v.id AND m.deleted_at IS NULL
     WHERE m.id IS NULL
        OR m.path <> v.path OR m.sort_order <> v.sort_order OR m.title <> v.title
        OR m.icon <> v.icon OR m.component <> v.component
        OR m.parent_id <> own_dir OR m.menu_type <> 2
        OR m.is_visible IS NOT TRUE OR m.status <> 1;
    IF n_bad > 0 THEN
        RAISE EXCEPTION 'V0387 自检失败：% 个页面菜单的形态与设计 §6.1 不符（path/父级/sort/title/icon/component）', n_bad;
    END IF;

    -- 5.4 30 个按钮形态（type 3 / 不可见 / name = auth_code / 前缀 / 父页面挂对）
    SELECT count(*) INTO n_bad
      FROM sys_menu b
     WHERE b.id = ANY(own_btns)
       AND (b.deleted_at IS NOT NULL OR b.menu_type <> 3 OR b.is_visible IS NOT FALSE
            OR b.name <> b.auth_code OR b.auth_code NOT LIKE 'business:hr:%'
            OR b.path IS NOT NULL
            OR b.parent_id <> (CASE
                   WHEN b.auth_code LIKE 'business:hr:employee:%'
                     OR b.auth_code = 'business:hr:contract:manage' THEN '20000000-0000-4000-8000-00000000e001'::uuid
                   WHEN b.auth_code LIKE 'business:hr:attendance:%' THEN '20000000-0000-4000-8000-00000000e002'::uuid
                   WHEN b.auth_code LIKE 'business:hr:leave:%'      THEN '20000000-0000-4000-8000-00000000e003'::uuid
                   WHEN b.auth_code LIKE 'business:hr:salary-item:%' THEN '20000000-0000-4000-8000-00000000e005'::uuid
                   WHEN b.auth_code LIKE 'business:hr:salary:%'     THEN '20000000-0000-4000-8000-00000000e004'::uuid
                   ELSE NULL END));
    IF n_bad > 0 THEN
        RAISE EXCEPTION 'V0387 自检失败：% 个按钮权限点的形态不对（type/is_visible/name=auth_code/前缀/父页面）', n_bad;
    END IF;

    -- 5.5 30 个权限码 + 5 个页面码 + 目录 path 全库唯一（AGENTS 约定 7 + menu_invariants ④）
    SELECT count(*), string_agg(m.auth_code || ' → ' || m.id::text, '; ')
      INTO n_dup, sample
      FROM sys_menu m
     WHERE m.deleted_at IS NULL
       AND m.auth_code <> ''          -- 目录行 auth_code 是空串，不能参与"同码"比对（会命中所有目录）
       AND m.auth_code IN (SELECT auth_code FROM sys_menu WHERE id = ANY(all_ids) AND auth_code <> '')
       AND NOT (m.id = ANY(all_ids));
    IF n_dup > 0 THEN
        RAISE EXCEPTION 'V0387 自检失败：% 条 auth_code 被本迁移之外的菜单占用（授权树会分叉）：%', n_dup, sample;
    END IF;

    -- 5.6 同父 sort 唯一（本目录下 5 页、5 个页面各自的按钮；全库口径由 menu_invariants ⑦ 兜）
    SELECT count(*) INTO n_dup FROM (
        SELECT parent_id, sort_order FROM sys_menu
         WHERE deleted_at IS NULL AND parent_id = ANY(ARRAY[own_dir] || own_pages)
         GROUP BY parent_id, sort_order HAVING count(*) > 1
    ) d;
    IF n_dup > 0 THEN
        RAISE EXCEPTION 'V0387 自检失败：人力资源下 % 组 sort_order 撞车（侧栏顺序会漂）', n_dup;
    END IF;

    -- 5.7 title 长度（1/2 级 ≤12，规范目标 2-6 字）+ icon 非空
    SELECT count(*) INTO n_bad FROM sys_menu
     WHERE deleted_at IS NULL AND id = ANY(ARRAY[own_dir] || own_pages)
       AND (length(title) > 12 OR icon IS NULL OR btrim(icon) = '' OR btrim(title) = '');
    IF n_bad > 0 THEN
        RAISE EXCEPTION 'V0387 自检失败：% 个目录/页面 title 超 12 字或 icon/title 为空', n_bad;
    END IF;

    -- 5.8 角色 HR_MANAGER 在位且形态对
    SELECT count(*) INTO n_bad FROM sys_role
     WHERE role_code = 'HR_MANAGER' AND deleted_at IS NULL AND status = 1
       AND role_name = '人事经理' AND data_scope = 3;
    IF n_bad <> 1 THEN
        RAISE EXCEPTION 'V0387 自检失败：HR_MANAGER 角色应有且仅有 1 个活角色（人事经理/启用/data_scope=3），实际 %', n_bad;
    END IF;

    -- 5.9 授权条数（存在的角色必须拿满应有的条数）
    FOREACH rc IN ARRAY ARRAY['SUPER_ADMIN','HR_MANAGER','FINANCE_MANAGER','DEPT_LEADER','EMPLOYEE']
    LOOP
        IF NOT EXISTS (SELECT 1 FROM sys_role WHERE role_code = rc AND deleted_at IS NULL) THEN
            RAISE NOTICE 'V0387：角色 % 不存在 → 跳过它的授权自检', rc;
            CONTINUE;
        END IF;
        expect := CASE rc
                    WHEN 'HR_MANAGER'     THEN 36
                    WHEN 'FINANCE_MANAGER' THEN 5
                    WHEN 'DEPT_LEADER'     THEN 5
                    WHEN 'EMPLOYEE'        THEN 5
                    ELSE 36
                  END;
        SELECT count(*) INTO n_grant
          FROM sys_role_menu rm JOIN sys_role r ON r.id = rm.role_id
         WHERE r.role_code = rc AND r.deleted_at IS NULL AND rm.menu_id = ANY(all_ids);
        IF n_grant <> expect THEN
            RAISE EXCEPTION 'V0387 自检失败：角色 % 应持有 % 条人力资源菜单授权，实际 %', rc, expect, n_grant;
        END IF;

        -- 无孤儿：授了按钮 → 父页面必须也授；授了页面 → 目录必须也授
        SELECT count(*) INTO n_orphan
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id
          JOIN sys_menu b ON b.id = rm.menu_id
         WHERE r.role_code = rc AND r.deleted_at IS NULL AND b.id = ANY(own_btns)
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm2
                            WHERE rm2.role_id = rm.role_id AND rm2.menu_id = b.parent_id);
        IF n_orphan > 0 THEN
            RAISE EXCEPTION 'V0387 自检失败：角色 % 有 % 个按钮是孤儿（父页面未授，进不去列表）', rc, n_orphan;
        END IF;

        SELECT count(*) INTO n_orphan
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id
         WHERE r.role_code = rc AND r.deleted_at IS NULL
           AND rm.menu_id = ANY(own_pages)
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm2
                            WHERE rm2.role_id = rm.role_id AND rm2.menu_id = own_dir);
        IF n_orphan > 0 THEN
            RAISE EXCEPTION 'V0387 自检失败：角色 % 有 % 个页面缺祖先目录授权（非超管侧栏整棵子树会被丢掉）', rc, n_orphan;
        END IF;
    END LOOP;

    -- 5.10 菜单总数（与 menu_invariants ⑤ 同口径 520；超了先评估再调阈值）
    SELECT count(*) INTO n_total FROM sys_menu WHERE deleted_at IS NULL;
    IF n_total > 520 THEN
        RAISE EXCEPTION 'V0387 自检失败：菜单总数 % 超过 menu_invariants ⑤ 的上限 520（先评估再调阈值）', n_total;
    END IF;

    RAISE NOTICE 'V0387：人力资源 1 目录 + 5 页面 + 30 按钮已就绪，菜单总数 % 条（上限 520）', n_total;
END $$;
