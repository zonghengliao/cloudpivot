-- ============================================================================
-- V0462：批次成本页 · 菜单（M5-07【M5·成本】分批法 前端批次成本页）
--
-- 本迁移只碰 `sys_menu` / `sys_role_menu`。零新表、零新列、零新权限码。
--
-- ── 为什么这一页现在才种菜单（M5-02…M5-06 都刻意没种）────────────────────
--   `cost_collect_sheet_dto.rs::PERM_LIST` 与 `cost_batch_collect_dto.rs` 的模块头
--   都写明了同一条：**页面属 M5-07**。前六卡只做后端，先插一条指向不存在组件的菜单行，
--   侧栏点开就是 404（`system::menu::tests::n08` 真库档也会当场红）。
--   本卡把 `business/erp/cost-batch/index.vue` 落地，菜单才有意义。
--
-- ── 权限码：**复用** `business:erp:cost-object:list`（不新增码）────────────────
--   8 条接口的读码全是它（M5-02/03/04/05 四个模块头的决定），本页每一行都是一个成本对象，
--   每一列都能用同一个码从成本对象页 / 明细里读出来 —— 新开一个码不改变任何人能看到什么，
--   只会多一条「有菜单、无接口」的死码（`permission_audit_tests::n14` 判红）。
--   写动作（归集 / 结转）走 `business:erp:cost-period:calc`，由前端按钮级 `v-privilege` 判，
--   该码在 V0378 已种、已授权，本迁移不重复种。
--   ⇒ 所以本迁移**没有按钮行**：本页没有任何一个动作使用「新码」，插按钮行只会造重复码。
--
-- ── 挂载点（按 V0390 的现状，不按设计稿）─────────────────────────────────
--   父目录「成本核算」（`menu_type=1`、`path='/business/erp/cost'`，V0390 建）下已有
--   sort = 56（成本方法）/ 60 / 62 / 64 / 66 / 68 六个页面。
--   本页取 `sort_order = 70`：
--   · 排在「成本核算」之后 —— 批次成本看的是**归集结果**，顺序与数据流一致
--     （先定方法 → 再核算 → 再看批成本明细）；
--   · 70 与既有 56…68 无冲突（AGENTS 约定 12「同父 sort 唯一」），且 69 留空给后续插队。
--   父目录按 **path 查**而不是写死 UUID：V0390 建目录时用 `gen_random_uuid()`，
--   各环境 id 不同，写死会让别的环境迁移直接失败。
--
-- ── 菜单口径（AGENTS 约定 12）────────────────────────────────────────────
--   name  = 页面中文短名（与 title 一致，与 V0460 同款）
--   title = 「批次成本」4 字，不带标点 / 说明 / 章节号（≤12 硬顶）
--   icon  = `chart-bar`（TDesign 真实图标：node_modules/tdesign-icons-vue-next/esm/components/chart-bar.js）
--   path  = `/business/erp/cost-batch`（menu_type=2 唯一，menu_invariants ④）
--   auth_code：页面行承载 `…:cost-object:list` —— ERP 尚未做页面码 / 接口码解耦
--              （解耦只试点在 HR，见 AGENTS 约定 14 / docs/architecture/PERM-01）
--
-- ── 角色授权（**派生**，不手抄矩阵）──────────────────────────────────────
--   「谁能读成本对象，谁就能看这一页」：把**当前已持有** `business:erp:cost-object:list`
--   的角色全部映射到本页行。手抄角色清单会在角色矩阵调整后立刻漂移
--   （V0401 解耦那次的教训），派生则永远与码的授权面一致。
--   另外显式补 `SUPER_ADMIN`：超管本可 `*:*:*` 短路，显式授予只为角色树上能看清范围
--   （本仓没有 `ADMIN` 这个角色码，别照抄 V0460 的 ADMIN —— 那一行在空库上是 0 行）。
--
-- ── 幂等 ────────────────────────────────────────────────────────────────
--   菜单 `NOT EXISTS (id)` 判定（不覆盖人工改名）；授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`。
--   重放 = 0 行插入；末尾自检只在「形态不符」时抛异常。
--
-- ── 回滚 ────────────────────────────────────────────────────────────────
--   V0462.down.sql：先删本迁移授的授权行（按本页 menu_id）→ 删本页行；
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
     WHERE deleted_at IS NULL AND path = '/business/erp/cost-batch'
       AND id <> '20000000-0000-4000-8000-00000000d201'::uuid;
    IF v_conflict > 0 THEN
        RAISE EXCEPTION 'V0462：path /business/erp/cost-batch 已被其它菜单占用（% 行），先清理再迁移', v_conflict;
    END IF;

    -- §0.2 复用的读码必须已经种过（否则本页会是一条「有菜单、无接口」的死码）
    SELECT count(*) INTO v_code_rows
      FROM sys_menu
     WHERE deleted_at IS NULL AND auth_code = 'business:erp:cost-object:list';
    IF v_code_rows = 0 THEN
        RAISE EXCEPTION 'V0462：读码 business:erp:cost-object:list 在 sys_menu 里不存在（V0378 未应用？）';
    END IF;

    -- §0.3 父目录必须存在（V0390 建；空库首启时 V0390 已先跑）
    SELECT id INTO v_dir_id
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 1 AND path = '/business/erp/cost'
     LIMIT 1;
    IF v_dir_id IS NULL THEN
        RAISE EXCEPTION 'V0462：找不到「成本核算」目录（path=/business/erp/cost, menu_type=1），V0390 未应用？';
    END IF;

    -- §0.4 同父 sort 不许撞（AGENTS 约定 12；撞了侧栏顺序每次刷新都会变）
    SELECT count(*) INTO v_conflict
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND parent_id = v_dir_id AND sort_order = 70
       AND id <> '20000000-0000-4000-8000-00000000d201'::uuid;
    IF v_conflict > 0 THEN
        RAISE EXCEPTION 'V0462：成本核算目录下 sort_order=70 已被占用（% 行），换一个空位再迁移', v_conflict;
    END IF;

    RAISE NOTICE 'V0462：挂载点为「成本核算」目录 %（sort 70）', v_dir_id;
END $$;

-- ---------------------------------------------------------------------------
-- §1 页面菜单（menu_type = 2，进侧栏）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT '20000000-0000-4000-8000-00000000d201'::uuid, '批次成本', 2,
       'business:erp:cost-object:list',
       '/business/erp/cost-batch', d.id, 'business/erp/cost-batch/index',
       70, 1, 'chart-bar', '批次成本', TRUE, 'business'
  FROM sys_menu d
 WHERE d.deleted_at IS NULL AND d.menu_type = 1 AND d.path = '/business/erp/cost'
   AND NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = '20000000-0000-4000-8000-00000000d201'::uuid);

-- ---------------------------------------------------------------------------
-- §2 角色授权（派生：持有读码的角色 + 超管 / 管理员）
-- ---------------------------------------------------------------------------
-- §2.1 已持有 `business:erp:cost-object:list` 的角色 → 本页
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT DISTINCT rm.role_id, '20000000-0000-4000-8000-00000000d201'::uuid
  FROM sys_role_menu rm
  JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
  JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
 WHERE m.auth_code = 'business:erp:cost-object:list'
   AND m.id <> '20000000-0000-4000-8000-00000000d201'::uuid
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- §2.2 超管（超管本可通配短路，显式授予只为角色树上能看清范围）
--
-- 只写 `SUPER_ADMIN`：本仓 `sys_role` 里没有 `ADMIN` 这个码（生产活角色 12 个，见
-- `scripts/verify/role_code_gate.py` 的白名单），写进去只会**静默 0 行**并被
-- 迁移授权段闸判红（V0462 首版就被这条拦住过）。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, '20000000-0000-4000-8000-00000000d201'::uuid
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
     WHERE m.deleted_at IS NULL AND m.id = '20000000-0000-4000-8000-00000000d201'::uuid
       AND m.menu_type = 2 AND m.path = '/business/erp/cost-batch'
       AND m.auth_code = 'business:erp:cost-object:list'
       AND m.title = '批次成本' AND length(m.title) <= 12
       AND m.icon = 'chart-bar' AND m.component = 'business/erp/cost-batch/index'
       AND m.is_visible IS TRUE AND m.status = 1
       AND m.sort_order = 70 AND d.path = '/business/erp/cost' AND d.menu_type = 1;
    IF v_page <> 1 THEN
        RAISE EXCEPTION 'V0462 自检失败：批次成本页面菜单形态不符（% 行命中期望）', v_page;
    END IF;

    -- 3.2 同 path 的页面只有这一条（menu_invariants ④）
    SELECT count(*) INTO v_path_dup
      FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type = 2 AND path = '/business/erp/cost-batch';
    IF v_path_dup <> 1 THEN
        RAISE EXCEPTION 'V0462 自检失败：path /business/erp/cost-batch 有 % 条页面行（应为 1）', v_path_dup;
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
        RAISE EXCEPTION 'V0462 自检失败：成本核算目录下 sort_order 有重复（% 组）', v_sort_dup;
    END IF;

    -- 3.4 无孤儿：本页的 parent 必须是存在的目录（3.1 已查，这里按 parent 再兜一次）
    SELECT count(*) INTO v_orphan
      FROM sys_menu m
     WHERE m.deleted_at IS NULL AND m.id = '20000000-0000-4000-8000-00000000d201'::uuid
       AND NOT EXISTS (SELECT 1 FROM sys_menu p
                        WHERE p.id = m.parent_id AND p.deleted_at IS NULL AND p.menu_type = 1);
    IF v_orphan > 0 THEN
        RAISE EXCEPTION 'V0462 自检失败：批次成本页面没有父目录（孤儿页面）';
    END IF;

    -- 3.5 授权面不落后于读码：持有 cost-object:list 的角色都必须也能看到本页
    SELECT count(*) INTO v_ungranted
      FROM (
        SELECT DISTINCT rm.role_id
          FROM sys_role_menu rm
          JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
          JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
         WHERE m.auth_code = 'business:erp:cost-object:list'
           AND m.id <> '20000000-0000-4000-8000-00000000d201'::uuid
      ) AS holders
     WHERE NOT EXISTS (
        SELECT 1 FROM sys_role_menu rm2
         WHERE rm2.role_id = holders.role_id
           AND rm2.menu_id = '20000000-0000-4000-8000-00000000d201'::uuid
     );
    IF v_ungranted > 0 THEN
        RAISE EXCEPTION 'V0462 自检失败：% 个持有 cost-object:list 的角色没被授到「批次成本」页', v_ungranted;
    END IF;

    -- 3.6 至少授给 1 个角色（反向自检：派生 SQL 万一连不上，别静默产出 0 行授权）
    SELECT count(*) INTO v_granted
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-4000-8000-00000000d201'::uuid;
    IF v_granted = 0 THEN
        RAISE EXCEPTION 'V0462 自检失败：批次成本页 0 行授权（派生授权没生效）';
    END IF;

    RAISE NOTICE 'V0462 完成：批次成本页已挂到「成本核算」目录 sort 70（读码复用 cost-object:list，% 个角色已授）', v_granted;
END $$;
