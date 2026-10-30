-- V0128：补「审批流」菜单（目录 + 2 个子菜单）
--
-- 背景：D1 的 V0121 只种了「流程定义」一个顶级菜单 + 11 个按钮级权限，
--       D2 的 V0124 补了实例侧权限码，但都没有种「发起审批」「审批中心」的菜单节点。
--       前端路由（router/index.ts:291/298）和页面都在，只是侧边栏没入口。
--
-- ID 分配（已核对 0790 被 content 占用，0860-0870 空闲）：
--   0860 = 审批流（目录，menu_type=1）
--   0861 = 发起审批（menu_type=2）
--   0862 = 审批中心（menu_type=2）

-- ① 父目录「审批流」
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component, sort_order, status, icon, title, is_frame, is_cache, is_visible, hidden_expression, module_code, created_at, updated_at)
VALUES (
    '20000000-0000-0000-0000-000000000860', '审批流', 1, '', '/flow', NULL, NULL,
    55, 1, 'task-checked', '审批流', false, false, true, NULL, 'business', now(), now()
)
ON CONFLICT (id) DO NOTHING;

-- ② 把已有的「流程定义」挂到父目录下
UPDATE sys_menu
   SET parent_id = '20000000-0000-0000-0000-000000000860',
       sort_order = 10,
       updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000780';

-- ③ 「发起审批」
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component, sort_order, status, icon, title, is_frame, is_cache, is_visible, hidden_expression, module_code, created_at, updated_at)
VALUES (
    '20000000-0000-0000-0000-000000000861', '发起审批', 2, 'business:flow:instance:create', '/flow/start',
    '20000000-0000-0000-0000-000000000860', 'business/flow/start/index',
    20, 1, 'add-circle', '发起审批', false, false, true, NULL, 'business', now(), now()
)
ON CONFLICT (id) DO NOTHING;

-- ④ 「审批中心」
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component, sort_order, status, icon, title, is_frame, is_cache, is_visible, hidden_expression, module_code, created_at, updated_at)
VALUES (
    '20000000-0000-0000-0000-000000000862', '审批中心', 2, 'business:flow:instance:read', '/flow/task',
    '20000000-0000-0000-0000-000000000860', 'business/flow/task/index',
    30, 1, 'task', '审批中心', false, false, true, NULL, 'business', now(), now()
)
ON CONFLICT (id) DO NOTHING;

-- ⑤ 授权给所有未删除的角色（不仅超管，普通员工也要能看审批中心）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND m.id IN (
       '20000000-0000-0000-0000-000000000860',
       '20000000-0000-0000-0000-000000000861',
       '20000000-0000-0000-0000-000000000862'
   )
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ⑥ 自检：3 个菜单必须存在且 parent 正确
DO $$
DECLARE
    n_menu integer;
    n_child integer;
BEGIN
    SELECT count(*) INTO n_menu
      FROM sys_menu
     WHERE id IN (
         '20000000-0000-0000-0000-000000000860',
         '20000000-0000-0000-0000-000000000861',
         '20000000-0000-0000-0000-000000000862'
     )
       AND deleted_at IS NULL;
    IF n_menu <> 3 THEN
        RAISE EXCEPTION 'V0128 自检失败：审批流菜单应有 3 个，实际 %', n_menu;
    END IF;

    SELECT count(*) INTO n_child
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000000860'
       AND deleted_at IS NULL;
    IF n_child <> 3 THEN
        RAISE EXCEPTION 'V0128 自检失败：审批流下应有 3 个子菜单（流程定义+发起+中心），实际 %', n_child;
    END IF;
END $$;
