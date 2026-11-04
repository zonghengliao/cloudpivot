-- ======================================================
-- 账务不变量巡检看板 · 菜单与权限点（P2-7）
--
-- 节点（父节点 = 支撑功能目录 `…0022`）：
--   …0600 页面节点 `/support/invariant`（menu_type 2，可见）      —— 权限码 support:invariant:list
--   …0601 按钮「立即执行一次」（menu_type 3，隐藏）               —— 权限码 support:invariant:run
--
-- ── 为什么用种子（S__）而不是版本化迁移 ───────────────────────
-- 迁移执行顺序是「版本化 → 种子」，空库上版本化迁移跑完时 `sys_role` 还没有行
-- （角色由 `S__role_data.sql` 创建），`INSERT … SELECT FROM sys_role` 会命中 0 行 →
-- 菜单建好了却没人被授权。文件名必须排在 `S__role_data.sql` 之后（字典序 `s` > `r`）。
--
-- ── 授权口径 ───────────────────────────────────────────────
-- 1. `support:invariant:list` → **所有未软删角色**
--    看板是只读的事实展示（账务健康度），看不到反而是信息不对称。
-- 2. `support:invariant:run` → **只授 SUPER_ADMIN**
--    「立即跑一次巡检」会拉起子进程扫全库，属于运维动作；与其它「改配置类」权限同口径。
--
-- ── 幂等 ──────────────────────────────────────────────────
-- id 固定 UUID + `ON CONFLICT (id) DO UPDATE`：重跑会自愈标题 / 排序 / 可见性；
-- 授权走 `NOT EXISTS` 预判，重复执行不报错。
-- ======================================================

-- ---------- 1) 菜单节点（页面 + 按钮） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
SELECT v.id, v.name, v.menu_type, v.auth_code, v.path, v.parent_id, v.component,
       v.sort_order, v.status, v.icon, v.title, v.is_visible, v.module_code
  FROM (VALUES
    ('10000000-0000-0000-0000-000000000600'::uuid, 'support:invariant', 2,
     'support:invariant:list', '/support/invariant',
     '10000000-0000-0000-0000-000000000022'::uuid, 'support/invariant/index',
     70, 1, 'chart', '账务巡检', TRUE, 'support'),
    ('10000000-0000-0000-0000-000000000601'::uuid, 'support:invariant:run', 3,
     'support:invariant:run', NULL,
     '10000000-0000-0000-0000-000000000600'::uuid, NULL,
     10, 1, NULL, '立即执行一次', FALSE, 'support')
  ) AS v(id, name, menu_type, auth_code, path, parent_id, component,
         sort_order, status, icon, title, is_visible, module_code)
ON CONFLICT (id) DO UPDATE
   SET name       = EXCLUDED.name,
       menu_type  = EXCLUDED.menu_type,
       auth_code  = EXCLUDED.auth_code,
       path       = EXCLUDED.path,
       parent_id  = EXCLUDED.parent_id,
       component  = EXCLUDED.component,
       sort_order = EXCLUDED.sort_order,
       status     = EXCLUDED.status,
       icon       = EXCLUDED.icon,
       title      = EXCLUDED.title,
       is_visible = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at = NULL;

-- ---------- 2) 读权限：授予所有未软删角色 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, '10000000-0000-0000-0000-000000000600'::uuid, CURRENT_TIMESTAMP
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id
              AND granted.menu_id = '10000000-0000-0000-0000-000000000600'::uuid
       )
ON CONFLICT DO NOTHING;

-- ---------- 3) 执行权限：只授 SUPER_ADMIN ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, '10000000-0000-0000-0000-000000000601'::uuid, CURRENT_TIMESTAMP
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id
              AND granted.menu_id = '10000000-0000-0000-0000-000000000601'::uuid
       )
ON CONFLICT DO NOTHING;

-- ---------- 4) 自检（fail-closed）----------
-- 页面节点必须存在、可见、且有权限码；两个权限点必须能被后端 #[nik::permission] 命中。
DO $$
DECLARE
    missing INT;
BEGIN
    SELECT count(*) INTO missing
      FROM (VALUES ('10000000-0000-0000-0000-000000000600'::uuid, 'support:invariant:list'),
                   ('10000000-0000-0000-0000-000000000601'::uuid, 'support:invariant:run')) AS want(id, code)
     WHERE NOT EXISTS (
               SELECT 1 FROM sys_menu m
                WHERE m.id = want.id AND m.auth_code = want.code AND m.deleted_at IS NULL
           );
    IF missing > 0 THEN
        RAISE EXCEPTION '账务巡检菜单未建全：缺少 % 个权限节点', missing;
    END IF;
END $$;
