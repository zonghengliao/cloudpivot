-- ============================================================================
-- V0471：在制品页 · 菜单（M5-25【M5·成本】在制品：前端页 · 录入 + 余额）
--
-- 本迁移只碰 `sys_menu` / `sys_role_menu`。零新表、零新列、零新权限码。
--
-- ── 为什么这个页面要种菜单（M5-20…M5-24 交付时明确留给本卡）──────────────
--   M5-20/21/22/23 四张卡交付的都是**纯后端**（四条接口，模块头各自写明
--   「前端页在 M5-25，那时才有菜单」）：
--     · GET/PUT /business/erp/cost-object/{id}/operations   （M5-21 工序完工率）
--     · GET     /business/erp/cost-wip-equiv                （M5-20 约当产量，另一页消费）
--     · GET     /business/erp/cost-wip-alloc                （M5-22 分配，另一页消费）
--     · GET     /business/erp/cost-wip-balance              （M5-23 期末余额 + 结转核对）
--   本卡把 `business/erp/cost-wip/index.vue`（两个页签：完工率录入 / 在制余额）落地，
--   菜单才有意义 —— 先插一条指向不存在组件的菜单行，侧栏点开就是 404
--   （`system::menu::tests::n08` 真库档当场红；与 V0464 / V0466 同一条理由链）。
--
-- ── 权限码：**复用** `business:erp:cost-object:list`（不新增码）──────────────
--   ① 与接口闸逐字一致：`cost_wip_pct_dto::PERM_READ`、
--      `cost_wip_balance_dto::PERM_LIST` 都是 `business:erp:cost-object:list`
--      （两处后端单测钉住不漂移）。菜单挂别的码会造出「菜单看得见、接口 403」的错位。
--   ② 本页有**写动作**（保存工序完工率），它的码是 `business:erp:cost-object:update`，
--      该码已由 V0378 的「修改成本对象」按钮行（`…c102`）承载 —— 所以本迁移
--      **不加按钮行**：写动作不在菜单层重复声明权限，判权由前端 `v-privilege` + 后端注解完成。
--   ③ 新开 `business:erp:cost-wip:*` 属于「页面码 / 接口码解耦」（AGENTS 约定 14 / PERM-01），
--      ERP 全域尚未解耦、只在 HR 试点；真要解耦得**同时**改后端注解与角色矩阵，
--      那是独立的一张卡 —— 本卡范围是「在制品前端页」，不夹带。
--
-- ── 挂载点（按 V0390 / V0460 / V0462 / V0464 / V0466 的现状，不按设计稿）────────
--   父目录「成本核算」（`menu_type=1`、`path='/business/erp/cost'`）下已有
--   sort = 56（成本方法）/ 60 / 62 / 64 / 66 / 68 / 70（批次成本）/ 72（标准成本）/
--   74（差异分析）八个页面。本页取 `sort_order = 76`：
--   · 排在「差异分析」之后 —— 差异是事后分析，在制品是**期末存量的结账视角**，后置更顺；
--   · 76 与既有 56…74 无冲突（AGENTS 约定 12「同父 sort 唯一」），75 留空给插队。
--   父目录按 **path 查**而不是写死 UUID：V0390 建目录时用 `gen_random_uuid()`，
--   各环境 id 不同，写死会让别的环境迁移直接失败。
--
-- ── 菜单口径（AGENTS 约定 12）────────────────────────────────────────────
--   name  = 页面中文短名（与 title 一致，与 V0460 / V0462 / V0464 / V0466 同款）
--   title = 「在制品」3 字，不带标点 / 说明 / 章节号（≤12 硬顶）
--   icon  = `chart-stacked`（TDesign 真实图标：
--           node_modules/tdesign-icons-vue-next/esm/components/chart-stacked.js）
--   path  = `/business/erp/cost-wip`（menu_type=2 唯一，menu_invariants ④）
--   component = `business/erp/cost-wip/index`（前端 `router/index.ts` 手工注册了同一条，
--               见「ERP 成本：在制品（M5-25）」那段注释 —— 漏注册就是点菜单 404）
--   auth_code：页面行承载 `…:cost-object:list` —— ERP 尚未做页面码 / 接口码解耦（同 V0464）。
--
-- ── 角色授权（**派生**，不手抄矩阵）──────────────────────────────────────
--   「谁能看成本对象，谁就能看在制品」：把**当前已持有** `business:erp:cost-object:list`
--   的角色全部映射到本页行。手抄角色清单会在角色矩阵调整后立刻漂移（V0401 解耦那次的教训），
--   派生则永远与码的授权面一致。
--   另外显式补 `SUPER_ADMIN`：超管本可 `*:*:*` 短路，显式授予只为角色树上能看清范围。
--
-- ── 幂等 ────────────────────────────────────────────────────────────────
--   菜单 `NOT EXISTS (id)` 判定（不覆盖人工改名）；授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`。
--   重放 = 0 行插入；末尾自检只在「形态不符」时抛异常。
--
-- ── 回滚 ────────────────────────────────────────────────────────────────
--   V0471.down.sql：先删本迁移授的授权行（按本页 menu_id）→ 删本页行；
--   页下若出现本迁移之外的子菜单 → 拒绝（说明有人挂过东西，别替人删）。
-- ============================================================================

DO $$
DECLARE
    v_dir_id    uuid;
    v_conflict  int;
    v_code_rows int;
BEGIN
    -- §0.1 同 path 不许被别人占着（menu_invariants ④ path 唯一）
    SELECT count(*) INTO v_conflict
      FROM sys_menu
     WHERE deleted_at IS NULL AND path = '/business/erp/cost-wip'
       AND id <> '20000000-0000-4000-8000-00000000d501'::uuid;
    IF v_conflict > 0 THEN
        RAISE EXCEPTION 'V0471：path /business/erp/cost-wip 已被其它菜单占用（% 行），先清理再迁移', v_conflict;
    END IF;

    -- §0.2 复用的读码必须已经种过（否则本页会是一条「有菜单、无接口」的死码）
    SELECT count(*) INTO v_code_rows
      FROM sys_menu
     WHERE deleted_at IS NULL AND auth_code = 'business:erp:cost-object:list';
    IF v_code_rows = 0 THEN
        RAISE EXCEPTION 'V0471：读码 business:erp:cost-object:list 在 sys_menu 里不存在（V0378 未应用？）';
    END IF;

    -- §0.3 父目录必须存在（V0390 建；空库首启时 V0390 已先跑）
    SELECT id INTO v_dir_id
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 1 AND path = '/business/erp/cost'
     LIMIT 1;
    IF v_dir_id IS NULL THEN
        RAISE EXCEPTION 'V0471：找不到「成本核算」目录（path=/business/erp/cost, menu_type=1），V0390 未应用？';
    END IF;

    -- §0.4 同父 sort 不许撞（AGENTS 约定 12；撞了侧栏顺序每次刷新都会变）
    SELECT count(*) INTO v_conflict
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND parent_id = v_dir_id AND sort_order = 76
       AND id <> '20000000-0000-4000-8000-00000000d501'::uuid;
    IF v_conflict > 0 THEN
        RAISE EXCEPTION 'V0471：成本核算目录下 sort_order=76 已被占用（% 行），换一个空位再迁移', v_conflict;
    END IF;

    RAISE NOTICE 'V0471：挂载点为「成本核算」目录 %（sort 76）', v_dir_id;
END $$;

-- ---------------------------------------------------------------------------
-- §1 页面菜单（menu_type = 2，进侧栏）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-4000-8000-00000000d501'::uuid, '在制品', 2,
       'business:erp:cost-object:list',
       '/business/erp/cost-wip', d.id, 'business/erp/cost-wip/index',
       76, 1, 'chart-stacked', '在制品', TRUE, 'business'
  FROM sys_menu d
 WHERE d.deleted_at IS NULL AND d.menu_type = 1 AND d.path = '/business/erp/cost'
   AND NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = '20000000-0000-4000-8000-00000000d501'::uuid);

-- ---------------------------------------------------------------------------
-- §2 角色授权（派生：持有读码的角色 + 超管）
-- ---------------------------------------------------------------------------
-- §2.1 已持有 `business:erp:cost-object:list` 的角色 → 本页
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT rm.role_id, '20000000-0000-4000-8000-00000000d501'::uuid
  FROM sys_role_menu rm
  JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
  JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
 WHERE m.auth_code = 'business:erp:cost-object:list'
   AND m.id <> '20000000-0000-4000-8000-00000000d501'::uuid
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- §2.2 超管（超管本可通配短路，显式授予只为角色树上能看清范围）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, '20000000-0000-4000-8000-00000000d501'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL AND r.role_code = 'SUPER_ADMIN'
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- §2.3 祖先闭包：拿到本页的角色，必须**连祖先目录一起拿到**
-- ---------------------------------------------------------------------------
-- `/system/auth/menus` 用 `utils::build_tree` 组装树 —— 父节点不在结果集里的节点会被
-- **整块丢掉**，而结果集只来自 `sys_role_menu`。所以「授了页面、没授祖先目录」＝
-- 侧栏里**什么都看不到**（不是灰、不是 403，是静默消失），AGENTS 约定 12 / HR 口径是硬约束。
-- 这里用递归 CTE 沿 parent_id 上溯，一代不落；已有授权的行 `ON CONFLICT DO NOTHING`。
WITH RECURSIVE holders AS (
    SELECT DISTINCT rm.role_id
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE rm.menu_id = '20000000-0000-4000-8000-00000000d501'::uuid
), up AS (
    SELECT m.id, m.parent_id
      FROM sys_menu m
     WHERE m.id = '20000000-0000-4000-8000-00000000d501'::uuid AND m.deleted_at IS NULL
    UNION ALL
    SELECT p.id, p.parent_id
      FROM sys_menu p
      JOIN up u ON p.id = u.parent_id
     WHERE p.deleted_at IS NULL
)
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT h.role_id, u.id
  FROM holders h
  CROSS JOIN up u
 WHERE u.id <> '20000000-0000-4000-8000-00000000d501'::uuid
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- §3 自检（任一不符 → 迁移失败；宁可启动失败也不要「半套菜单 / 半套授权」上线）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_page      integer;
    v_path_dup  integer;
    v_sort_dup  integer;
    v_orphan    integer;
    v_ungranted integer;
    v_granted   integer;
BEGIN
    -- 3.1 页面行 1 个、形态对（type / path / 父 / title / icon / component / 显形）
    SELECT count(*) INTO v_page
      FROM sys_menu m
      JOIN sys_menu d ON d.id = m.parent_id AND d.deleted_at IS NULL
     WHERE m.deleted_at IS NULL AND m.id = '20000000-0000-4000-8000-00000000d501'::uuid
       AND m.menu_type = 2 AND m.path = '/business/erp/cost-wip'
       AND m.auth_code = 'business:erp:cost-object:list'
       AND m.title = '在制品' AND length(m.title) <= 12
       AND m.icon = 'chart-stacked' AND m.component = 'business/erp/cost-wip/index'
       AND m.is_visible IS TRUE AND m.status = 1
       AND m.sort_order = 76 AND d.path = '/business/erp/cost' AND d.menu_type = 1;
    IF v_page <> 1 THEN
        RAISE EXCEPTION 'V0471 自检失败：在制品页面菜单形态不符（% 行命中期望）', v_page;
    END IF;

    -- 3.2 同 path 的页面只有这一条（menu_invariants ④）
    SELECT count(*) INTO v_path_dup
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND path = '/business/erp/cost-wip';
    IF v_path_dup <> 1 THEN
        RAISE EXCEPTION 'V0471 自检失败：path /business/erp/cost-wip 有 % 条页面行（应为 1）', v_path_dup;
    END IF;

    -- 3.3 同父 sort 唯一（AGENTS 约定 12）
    SELECT count(*) INTO v_sort_dup
      FROM (
        SELECT parent_id, sort_order FROM sys_menu
         WHERE deleted_at IS NULL AND menu_type = 2
           AND parent_id = (SELECT id FROM sys_menu WHERE deleted_at IS NULL
                              AND menu_type = 1 AND path = '/business/erp/cost' LIMIT 1)
         GROUP BY parent_id, sort_order HAVING count(*) > 1
      ) AS dup;
    IF v_sort_dup > 0 THEN
        RAISE EXCEPTION 'V0471 自检失败：成本核算目录下 sort_order 有重复（% 组）', v_sort_dup;
    END IF;

    -- 3.4 无孤儿：本页的 parent 必须是存在的目录
    SELECT count(*) INTO v_orphan
      FROM sys_menu m
     WHERE m.deleted_at IS NULL AND m.id = '20000000-0000-4000-8000-00000000d501'::uuid
       AND NOT EXISTS (SELECT 1 FROM sys_menu p
                        WHERE p.id = m.parent_id AND p.deleted_at IS NULL AND p.menu_type = 1);
    IF v_orphan > 0 THEN
        RAISE EXCEPTION 'V0471 自检失败：在制品页面没有父目录（孤儿页面）';
    END IF;

    -- 3.5 授权面不落后于读码：持有 cost-object:list 的角色都必须也能看到本页
    SELECT count(*) INTO v_ungranted
      FROM (
        SELECT DISTINCT rm.role_id
          FROM sys_role_menu rm
          JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
          JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
         WHERE m.auth_code = 'business:erp:cost-object:list'
           AND m.id <> '20000000-0000-4000-8000-00000000d501'::uuid
      ) AS holders
     WHERE NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm2
         WHERE rm2.role_id = holders.role_id
           AND rm2.menu_id = '20000000-0000-4000-8000-00000000d501'::uuid
     );
    IF v_ungranted > 0 THEN
        RAISE EXCEPTION 'V0471 自检失败：% 个持有 cost-object:list 的角色没被授到「在制品」页', v_ungranted;
    END IF;

    -- 3.6 至少授给 1 个角色（反向自检：派生 SQL 万一连不上，别静默产出 0 行授权）
    SELECT count(*) INTO v_granted
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-4000-8000-00000000d501'::uuid;
    IF v_granted = 0 THEN
        RAISE EXCEPTION 'V0471 自检失败：在制品页 0 行授权（派生授权没生效）';
    END IF;

    -- 3.7 祖先闭包完整：**每个**拿到本页的角色，沿 parent_id 上溯的每一代都必须已授
    SELECT count(*) INTO v_orphan
      FROM (
        WITH RECURSIVE up AS (
            SELECT m.id, m.parent_id
              FROM sys_menu m
             WHERE m.id = '20000000-0000-4000-8000-00000000d501'::uuid AND m.deleted_at IS NULL
            UNION ALL
            SELECT p.id, p.parent_id
              FROM sys_menu p
              JOIN up u ON p.id = u.parent_id
             WHERE p.deleted_at IS NULL
        )
        SELECT rm.role_id, u.id AS ancestor_id
          FROM sys_role_menu rm
          CROSS JOIN up u
          JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
         WHERE rm.menu_id = '20000000-0000-4000-8000-00000000d501'::uuid
           AND u.id <> '20000000-0000-4000-8000-00000000d501'::uuid
           AND NOT EXISTS (SELECT 1 FROM sys_role_menu x
                            WHERE x.role_id = rm.role_id AND x.menu_id = u.id)
      ) AS missing;
    IF v_orphan > 0 THEN
        RAISE EXCEPTION 'V0471 自检失败：有 % 处「授了页面没授祖先」，本页在非超管侧栏会静默消失', v_orphan;
    END IF;

    RAISE NOTICE 'V0471 完成：在制品页已挂到「成本核算」目录 sort 76（读码复用 cost-object:list，% 个角色已授，祖先闭包已补）', v_granted;
END $$;
