-- V0130：工作任务改为目录 + 三个子菜单（任务列表 / 任务看板 / 甘特图）
--
-- 背景（老大 2026-10-30）：
--   「工作任务菜单应该是一个目录，里面有任务列表、任务看板和甘特图三个菜单。」
--
-- 现状问题：
--   0760「工作任务」是 menu_type=2（菜单，不是目录），却带了子菜单 0770「甘特图」
--   —— 菜单节点挂子菜单在渲染时行为不确定（有的前端会忽略子节点）。
--
-- 目标结构：
--   0760 工作任务       menu_type=1（目录）  /task
--     ├─ 0900 任务列表  menu_type=2         /task/list   business/task/index
--     ├─ 0901 任务看板  menu_type=2         /task/board  business/task/index
--     └─ 0770 甘特图    menu_type=2         /task/gantt  business/task/gantt

-- ① 把「工作任务」改成目录
UPDATE sys_menu
   SET menu_type = 1,
       component = NULL,
       path = '/task',
       icon = 'task',
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000760';

-- ② 任务列表（0900）
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component, sort_order, status, icon, title, is_frame, is_cache, is_visible, hidden_expression, module_code, created_at, updated_at)
VALUES (
    '20000000-0000-0000-0000-000000000900', '任务列表', 2, 'business:task:read', '/task/list',
    '20000000-0000-0000-0000-000000000760', 'business/task/index',
    10, 1, 'list', '任务列表', false, false, true, '', 'business', now(), now()
)
ON CONFLICT (id) DO UPDATE
  SET name = EXCLUDED.name, menu_type = EXCLUDED.menu_type, auth_code = EXCLUDED.auth_code,
      path = EXCLUDED.path, parent_id = EXCLUDED.parent_id, component = EXCLUDED.component,
      sort_order = EXCLUDED.sort_order, status = EXCLUDED.status, icon = EXCLUDED.icon,
      title = EXCLUDED.title, is_visible = EXCLUDED.is_visible, updated_at = now();

-- ③ 任务看板（0901）
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component, sort_order, status, icon, title, is_frame, is_cache, is_visible, hidden_expression, module_code, created_at, updated_at)
VALUES (
    '20000000-0000-0000-0000-000000000901', '任务看板', 2, 'business:task:read', '/task/board',
    '20000000-0000-0000-0000-000000000760', 'business/task/index',
    20, 1, 'view-board', '任务看板', false, false, true, '', 'business', now(), now()
)
ON CONFLICT (id) DO UPDATE
  SET name = EXCLUDED.name, menu_type = EXCLUDED.menu_type, auth_code = EXCLUDED.auth_code,
      path = EXCLUDED.path, parent_id = EXCLUDED.parent_id, component = EXCLUDED.component,
      sort_order = EXCLUDED.sort_order, status = EXCLUDED.status, icon = EXCLUDED.icon,
      title = EXCLUDED.title, is_visible = EXCLUDED.is_visible, updated_at = now();

-- ④ 甘特图（0770）：确保 parent 正确、可见、排序第三
UPDATE sys_menu
   SET parent_id = '20000000-0000-0000-0000-000000000760',
       sort_order = 30,
       is_visible = TRUE,
       status = 1,
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000770';

-- ⑤ 授权给所有未删除角色（新加的两个子菜单）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND m.id IN (
       '20000000-0000-0000-0000-000000000900',
       '20000000-0000-0000-0000-000000000901'
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ⑥ 自检
DO $$
DECLARE
    v_type integer;
    v_comp text;
    n_child integer;
BEGIN
    -- ① 工作任务必须是目录
    SELECT menu_type, component INTO v_type, v_comp
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000000760';
    IF v_type <> 1 THEN
        RAISE EXCEPTION 'V0130 自检失败：工作任务应为目录(menu_type=1)，实际 %', v_type;
    END IF;
    IF v_comp IS NOT NULL THEN
        RAISE EXCEPTION 'V0130 自检失败：目录不应有 component，实际 %', v_comp;
    END IF;

    -- ② 必须有 3 个可见子菜单
    SELECT count(*) INTO n_child
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000000760'
       AND deleted_at IS NULL AND is_visible = TRUE AND status = 1;
    IF n_child <> 3 THEN
        RAISE EXCEPTION 'V0130 自检失败：工作任务下应有 3 个可见子菜单，实际 %', n_child;
    END IF;
END $$;
