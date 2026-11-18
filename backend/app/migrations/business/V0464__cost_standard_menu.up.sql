-- ============================================================================
-- V0464：标准成本维护页 · 菜单（M5-09【M5·成本】标准成本：维护页 CRUD · 批量导入）
--
-- 本迁移只碰 `sys_menu` / `sys_role_menu`。零新表、零新列、零新权限码。
--
-- ── 为什么 M5-08 没种菜单、本卡才种 ──────────────────────────────────────
--   M5-08 只交付后端 8 条接口 + 迁移（模块头写明「前端页面不在本卡」）。
--   先插一条指向不存在组件的菜单行，侧栏点开就是 404（`system::menu::tests::n08`
--   真库档当场红）。本卡把 `business/erp/cost-standard/index.vue` 落地，菜单才有意义。
--
-- ── 权限码：**复用** `business:erp:cost-method:list`（不新增码）───────────────
--   标准成本卡的 10 条接口（M5-08 的 8 条 + M5-09 的导入 / 模板 2 条）全部挂
--   `cost-method` 的四个码，理由见 `cost_standard_handler.rs` 模块头：
--   标准成本与「按什么方法核算」是同一类**全公司财务口径配置**，维护角色完全一致；
--   新开 `cost-standard:*` 在没有任何菜单行承载时就是死码
--   （`permission_audit_tests::n14` 判红），而先插一条指向不存在组件的菜单行
--   `n08` 又会红。M5-07（批次成本页）走的是同一个岔路口，同样选了复用。
--   ⇒ 所以本迁移**没有按钮行**：本页没有任何一个动作使用「新码」。
--
-- ── 挂载点（按 V0390 / V0462 的现状，不按设计稿）─────────────────────────
--   父目录「成本核算」（`menu_type=1`、`path='/business/erp/cost'`）下已有
--   sort = 56（成本方法）/ 60 / 62 / 64 / 66 / 68 / 70（批次成本）七个页面。
--   本页取 `sort_order = 72`：
--   · 排在「批次成本」之后 —— 标准成本是**口径输入**，先有批次实际成本才谈差异；
--   · 72 与既有 56…70 无冲突（AGENTS 约定 12「同父 sort 唯一」），71 留空给插队。
--   父目录按 **path 查**而不是写死 UUID：V0390 建目录时用 `gen_random_uuid()`，
--   各环境 id 不同，写死会让别的环境迁移直接失败。
--
-- ── 菜单口径（AGENTS 约定 12）────────────────────────────────────────────
--   name  = 页面中文短名（与 title 一致，与 V0460 / V0462 同款）
--   title = 「标准成本」4 字，不带标点 / 说明 / 章节号（≤12 硬顶）
--   icon  = `chart-line`（TDesign 真实图标：node_modules/tdesign-icons-vue-next/
--           esm/components/chart-line.js）
--   path  = `/business/erp/cost-standard`（menu_type=2 唯一，menu_invariants ④）
--   auth_code：页面行承载 `…:cost-method:list` —— ERP 尚未做页面码 / 接口码解耦
--              （解耦只试点在 HR，见 AGENTS 约定 14 / docs/architecture/PERM-01）
--
-- ── 角色授权（**派生**，不手抄矩阵）──────────────────────────────────────
--   「谁能看成本方法，谁就能维护标准成本」：把**当前已持有**
--   `business:erp:cost-method:list` 的角色全部映射到本页行。手抄角色清单会在
--   角色矩阵调整后立刻漂移（V0401 解耦那次的教训），派生则永远与码的授权面一致。
--   另外显式补 `SUPER_ADMIN`：超管本可 `*:*:*` 短路，显式授予只为角色树上能看清范围
--   （本仓没有 `ADMIN` 这个角色码，别照抄 V0460 的 ADMIN —— 那一行在空库上是 0 行）。
--
-- ── 幂等 ────────────────────────────────────────────────────────────────
--   菜单 `NOT EXISTS (id)` 判定（不覆盖人工改名）；授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`。
--   重放 = 0 行插入；末尾自检只在「形态不符」时抛异常。
--
-- ── 回滚 ────────────────────────────────────────────────────────────────
--   V0464.down.sql：先删本迁移授的授权行（按本页 menu_id）→ 删本页行；
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
     WHERE deleted_at IS NULL AND path = '/business/erp/cost-standard'
       AND id <> '20000000-0000-4000-8000-00000000d301'::uuid;
    IF v_conflict > 0 THEN
        RAISE EXCEPTION 'V0464：path /business/erp/cost-standard 已被其它菜单占用（% 行），先清理再迁移', v_conflict;
    END IF;

    -- §0.2 复用的读码必须已经种过（否则本页会是一条「有菜单、无接口」的死码）
    SELECT count(*) INTO v_code_rows
      FROM sys_menu
     WHERE deleted_at IS NULL AND auth_code = 'business:erp:cost-method:list';
    IF v_code_rows = 0 THEN
        RAISE EXCEPTION 'V0464：读码 business:erp:cost-method:list 在 sys_menu 里不存在（V0460 未应用？）';
    END IF;

    -- §0.3 父目录必须存在（V0390 建；空库首启时 V0390 已先跑）
    SELECT id INTO v_dir_id
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 1 AND path = '/business/erp/cost'
     LIMIT 1;
    IF v_dir_id IS NULL THEN
        RAISE EXCEPTION 'V0464：找不到「成本核算」目录（path=/business/erp/cost, menu_type=1），V0390 未应用？';
    END IF;

    -- §0.4 同父 sort 不许撞（AGENTS 约定 12；撞了侧栏顺序每次刷新都会变）
    SELECT count(*) INTO v_conflict
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND parent_id = v_dir_id AND sort_order = 72
       AND id <> '20000000-0000-4000-8000-00000000d301'::uuid;
    IF v_conflict > 0 THEN
        RAISE EXCEPTION 'V0464：成本核算目录下 sort_order=72 已被占用（% 行），换一个空位再迁移', v_conflict;
    END IF;

    RAISE NOTICE 'V0464：挂载点为「成本核算」目录 %（sort 72）', v_dir_id;
END $$;

-- ---------------------------------------------------------------------------
-- §1 页面菜单（menu_type = 2，进侧栏）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-4000-8000-00000000d301'::uuid, '标准成本', 2,
       'business:erp:cost-method:list',
       '/business/erp/cost-standard', d.id, 'business/erp/cost-standard/index',
       72, 1, 'chart-line', '标准成本', TRUE, 'business'
  FROM sys_menu d
 WHERE d.deleted_at IS NULL AND d.menu_type = 1 AND d.path = '/business/erp/cost'
   AND NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = '20000000-0000-4000-8000-00000000d301'::uuid);

-- ---------------------------------------------------------------------------
-- §2 角色授权（派生：持有读码的角色 + 超管）
-- ---------------------------------------------------------------------------
-- §2.1 已持有 `business:erp:cost-method:list` 的角色 → 本页
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT rm.role_id, '20000000-0000-4000-8000-00000000d301'::uuid
  FROM sys_role_menu rm
  JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
  JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
 WHERE m.auth_code = 'business:erp:cost-method:list'
   AND m.id <> '20000000-0000-4000-8000-00000000d301'::uuid
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- §2.2 超管（超管本可通配短路，显式授予只为角色树上能看清范围）
--
-- 只写 `SUPER_ADMIN`：本仓 `sys_role` 里没有 `ADMIN` 这个码（V0462 首版被这条闸拦过），
-- 写进去只会**静默 0 行**。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, '20000000-0000-4000-8000-00000000d301'::uuid
  FROM sys_role r
 WHERE r.deleted_at IS NULL AND r.role_code = 'SUPER_ADMIN'
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
     WHERE m.deleted_at IS NULL AND m.id = '20000000-0000-4000-8000-00000000d301'::uuid
       AND m.menu_type = 2 AND m.path = '/business/erp/cost-standard'
       AND m.auth_code = 'business:erp:cost-method:list'
       AND m.title = '标准成本' AND length(m.title) <= 12
       AND m.icon = 'chart-line' AND m.component = 'business/erp/cost-standard/index'
       AND m.is_visible IS TRUE AND m.status = 1
       AND m.sort_order = 72 AND d.path = '/business/erp/cost' AND d.menu_type = 1;
    IF v_page <> 1 THEN
        RAISE EXCEPTION 'V0464 自检失败：标准成本页面菜单形态不符（% 行命中期望）', v_page;
    END IF;

    -- 3.2 同 path 的页面只有这一条（menu_invariants ④）
    SELECT count(*) INTO v_path_dup
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND path = '/business/erp/cost-standard';
    IF v_path_dup <> 1 THEN
        RAISE EXCEPTION 'V0464 自检失败：path /business/erp/cost-standard 有 % 条页面行（应为 1）', v_path_dup;
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
        RAISE EXCEPTION 'V0464 自检失败：成本核算目录下 sort_order 有重复（% 组）', v_sort_dup;
    END IF;

    -- 3.4 无孤儿：本页的 parent 必须是存在的目录
    SELECT count(*) INTO v_orphan
      FROM sys_menu m
     WHERE m.deleted_at IS NULL AND m.id = '20000000-0000-4000-8000-00000000d301'::uuid
       AND NOT EXISTS (SELECT 1 FROM sys_menu p
                        WHERE p.id = m.parent_id AND p.deleted_at IS NULL AND p.menu_type = 1);
    IF v_orphan > 0 THEN
        RAISE EXCEPTION 'V0464 自检失败：标准成本页面没有父目录（孤儿页面）';
    END IF;

    -- 3.5 授权面不落后于读码：持有 cost-method:list 的角色都必须也能看到本页
    SELECT count(*) INTO v_ungranted
      FROM (
        SELECT DISTINCT rm.role_id
          FROM sys_role_menu rm
          JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
          JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
         WHERE m.auth_code = 'business:erp:cost-method:list'
           AND m.id <> '20000000-0000-4000-8000-00000000d301'::uuid
      ) AS holders
     WHERE NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm2
         WHERE rm2.role_id = holders.role_id
           AND rm2.menu_id = '20000000-0000-4000-8000-00000000d301'::uuid
     );
    IF v_ungranted > 0 THEN
        RAISE EXCEPTION 'V0464 自检失败：% 个持有 cost-method:list 的角色没被授到「标准成本」页', v_ungranted;
    END IF;

    -- 3.6 至少授给 1 个角色（反向自检：派生 SQL 万一连不上，别静默产出 0 行授权）
    SELECT count(*) INTO v_granted
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-4000-8000-00000000d301'::uuid;
    IF v_granted = 0 THEN
        RAISE EXCEPTION 'V0464 自检失败：标准成本页 0 行授权（派生授权没生效）';
    END IF;

    RAISE NOTICE 'V0464 完成：标准成本页已挂到「成本核算」目录 sort 72（读码复用 cost-method:list，% 个角色已授）', v_granted;
END $$;
