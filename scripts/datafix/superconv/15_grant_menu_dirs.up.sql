-- ============================================================================
-- SUPERCONV（P0-FIX-2B-C）· 阶段 1.5：给 5 个业务角色补「祖先目录」授权
-- ----------------------------------------------------------------------------
-- 为什么需要（这是本卡施工中发现的生产数据缺陷，不是可选项）
--   `system/auth/model.rs::load_user_menus_uncached` 对**非超管**只取「角色被直接授予的
--   menu_type IN (1,2) 行」；`utils::build_tree` 把 parent_id 不在节点集里的行丢进
--   children_map 但永远挂不上去 ⇒ **父目录没授权，子页面在侧边栏里直接消失**。
--
--   生产实测（2026-11-05 17:0x，只读）：5 个业务角色授予的 13 个页面里，有 9 处父目录
--   没被授权 —— `财务管理`（预算/费用报销/固定资产的根）、`采购管理`（盘点的根）、
--   `support`（invariant 的根）在生产上**只授给了 SUPER_ADMIN**。
--   也就是说：摘掉超管之后，这些角色名下的页面在侧边栏里一个都不会出现
--   —— laoliu 会看到「空侧边栏」，henry.liao 的「仍能看到业务菜单」验收也过不了。
--
-- 口径（刻意窄）
--   * 种子 = **已授予的页面**（menu_type=2）；沿链向上只补 **目录**（menu_type=1）；
--   * 不补「页面型父节点」、不碰按钮链 ⇒ 本阶段**不新增任何权限码**，只补导航容器
--     （目录的 auth_code 为空，`load_permissions_uncached` 里被 `auth_code <> ''` 过滤掉）；
--   * 只动 5 个业务角色（DEPT_LEADER / FINANCE_MANAGER / WAREHOUSE_MANAGER /
--     PURCHASE_MANAGER / GENERAL_MANAGER）；生产上这 5 个角色的持有者只有
--     laoliu_test / henry.liao（+ 本卡给 laoliu 授的），不改别的角色。
--
-- 幂等：重放 0 行（`NOT EXISTS` + `ON CONFLICT (role_id, menu_id) DO NOTHING`）。
-- 快照：本阶段插入的每一行写进 datafix_superconv_snapshot（batch=SUPERCONV-20261105-DIRS），
--       down 按 pk 精确删除。
-- ============================================================================
\set ON_ERROR_STOP on

CREATE TABLE IF NOT EXISTS datafix_superconv_snapshot (
  batch       text        NOT NULL,
  tbl         text        NOT NULL,
  pk          text        NOT NULL,
  row_data    jsonb       NOT NULL,
  captured_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (batch, tbl, pk)
);

DO $$
DECLARE
  v_batch text := 'SUPERCONV-20261105-DIRS';
  v_expect CONSTANT int := 9;
  v_add   int;
  v_ins   int;
  v_snap  int;
  v_list  text;
BEGIN
  CREATE TEMP TABLE _sc_dirs ON COMMIT DROP AS
  WITH RECURSIVE tr AS (
    SELECT id, role_code FROM sys_role
     WHERE role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
       AND deleted_at IS NULL
  ), seed AS (
    SELECT rm.role_id, m.id AS menu_id, m.parent_id
      FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id
     WHERE rm.role_id IN (SELECT id FROM tr) AND m.deleted_at IS NULL AND m.menu_type = 2
  ), up AS (
    SELECT s.role_id, s.parent_id AS node_id FROM seed s WHERE s.parent_id IS NOT NULL
    UNION
    SELECT u.role_id, m.parent_id FROM up u JOIN sys_menu m ON m.id = u.node_id
     WHERE m.parent_id IS NOT NULL AND m.deleted_at IS NULL
  )
  SELECT DISTINCT u.role_id, u.node_id AS menu_id
    FROM up u JOIN sys_menu d ON d.id = u.node_id AND d.deleted_at IS NULL AND d.status = 1
   WHERE d.menu_type = 1
     AND NOT EXISTS (SELECT 1 FROM sys_role_menu x WHERE x.role_id = u.role_id AND x.menu_id = u.node_id);

  SELECT count(*) INTO v_add FROM _sc_dirs;
  IF v_add = 0 THEN
    RAISE NOTICE '业务角色的祖先目录已齐，本脚本无变更（幂等重放）';
    RETURN;
  END IF;
  IF v_add <> v_expect THEN
    SELECT string_agg(t.role_code || '→' || d.name, ', ' ORDER BY t.role_code, d.name) INTO v_list
      FROM _sc_dirs a JOIN sys_menu d ON d.id = a.menu_id
      JOIN sys_role t ON t.id = a.role_id;
    RAISE EXCEPTION '待补目录期望 % 行，实际 % 行（%）；形状变了，停手复核菜单树', v_expect, v_add, v_list;
  END IF;

  INSERT INTO sys_role_menu (id, role_id, menu_id)
  SELECT gen_random_uuid(), a.role_id, a.menu_id FROM _sc_dirs a
  ON CONFLICT (role_id, menu_id) DO NOTHING;
  GET DIAGNOSTICS v_ins = ROW_COUNT;

  INSERT INTO datafix_superconv_snapshot (batch, tbl, pk, row_data)
  SELECT v_batch, 'sys_role_menu', rm.id::text, to_jsonb(rm)
    FROM sys_role_menu rm JOIN _sc_dirs a
      ON a.role_id = rm.role_id AND a.menu_id = rm.menu_id
  ON CONFLICT DO NOTHING;
  -- 断言：**本次插入的这 9 行**都有快照（不比对批次总行数 —— down 之后快照会留档，
  -- 重跑时批次里会同时存在历史行，比总数会误报）
  SELECT count(*) INTO v_snap
    FROM _sc_dirs a
    JOIN sys_role_menu rm ON a.role_id = rm.role_id AND a.menu_id = rm.menu_id
    JOIN datafix_superconv_snapshot s
      ON s.batch = v_batch AND s.tbl = 'sys_role_menu' AND s.pk = rm.id::text;
  IF v_snap <> v_ins THEN
    RAISE EXCEPTION '本次插入 % 行里只有 % 行有快照（SOP §1 红线：先改后补快照 = 违规）', v_ins, v_snap;
  END IF;

  SELECT string_agg(t.role_code || '→' || d.name, ', ' ORDER BY t.role_code, d.name) INTO v_list
    FROM _sc_dirs a JOIN sys_menu d ON d.id = a.menu_id JOIN sys_role t ON t.id = a.role_id;
  RAISE NOTICE '阶段 1.5 完成：补 % 行目录授权（%）', v_ins, v_list;
END $$;
