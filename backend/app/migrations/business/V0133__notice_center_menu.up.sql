-- V0133：新建「公告与消息」目录，把 4 个相关菜单收进去
--
-- 老大 2026-10-30 要求：
--   「公告与消息」目录，里面要有消息中心、公告中心的菜单
--   把内容管理里的「公告管理」和系统管理里的「消息通知」也都移过来
--   注意：页面顶部的消息菜单（铃铛）不要动
--
-- 移动清单：
--   0700 公告中心   顶级           → 0880 公告与消息
--   0010 消息中心   顶级(隐藏)      → 0880 公告与消息，并改 is_visible=TRUE
--   0710 公告管理   content(0790)  → 0880 公告与消息
--   0410 消息通知   system(0001)   → 0880 公告与消息（其子菜单 0020 发送通知 跟随）
--
-- 顶部铃铛 `<notice-bell />` 是前端组件，不在 sys_menu 里，不受影响。

-- ① 新建目录「公告与消息」
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component, sort_order, status, icon, title, is_frame, is_cache, is_visible, hidden_expression, module_code, created_at, updated_at)
VALUES (
    '20000000-0000-0000-0000-000000000880', 'notice-center', 1, '', '/notice-center', NULL, NULL,
    18, 1, 'notification', '公告与消息', false, false, true, '', 'business', now(), now()
)
ON CONFLICT (id) DO UPDATE
  SET name = EXCLUDED.name, menu_type = EXCLUDED.menu_type, title = EXCLUDED.title,
      path = EXCLUDED.path, parent_id = EXCLUDED.parent_id, component = EXCLUDED.component,
      sort_order = EXCLUDED.sort_order, status = EXCLUDED.status, icon = EXCLUDED.icon,
      is_visible = EXCLUDED.is_visible, updated_at = now();

-- ② 公告中心：顶级 → 目录下
UPDATE sys_menu
   SET parent_id = '20000000-0000-0000-0000-000000000880',
       sort_order = 10, is_visible = TRUE, updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000700';

-- ③ 消息中心：顶级(隐藏) → 目录下，并改为可见
UPDATE sys_menu
   SET parent_id = '20000000-0000-0000-0000-000000000880',
       sort_order = 20, is_visible = TRUE, updated_at = now()
 WHERE id = '23000000-0000-0000-0000-000000000010';

-- ④ 公告管理：content → 目录下
UPDATE sys_menu
   SET parent_id = '20000000-0000-0000-0000-000000000880',
       sort_order = 30, is_visible = TRUE, updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000710';

-- ⑤ 消息通知：system → 目录下（其子菜单 0020 发送通知 自动跟随）
UPDATE sys_menu
   SET parent_id = '20000000-0000-0000-0000-000000000880',
       sort_order = 40, is_visible = TRUE, updated_at = now()
 WHERE id = '20000000-0000-0000-0000-000000000410';

-- ⑥ 自检
DO $$
DECLARE
    n integer;
    v_visible boolean;
BEGIN
    -- 目录必须存在且可见
    SELECT count(*) INTO n FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000880'
       AND menu_type = 1 AND is_visible = TRUE AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0133 自检失败：公告与消息目录未正确创建';
    END IF;

    -- 4 个子菜单必须都在该目录下
    SELECT count(*) INTO n FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000000880'
       AND deleted_at IS NULL
       AND id IN (
           '20000000-0000-0000-0000-000000000700',
           '23000000-0000-0000-0000-000000000010',
           '20000000-0000-0000-0000-000000000710',
           '20000000-0000-0000-0000-000000000410'
       );
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0133 自检失败：目录下应有 4 个子菜单，实际 %', n;
    END IF;

    -- 消息中心必须可见（老大要求「里面要有消息中心菜单」）
    SELECT is_visible INTO v_visible FROM sys_menu
     WHERE id = '23000000-0000-0000-0000-000000000010';
    IF v_visible IS DISTINCT FROM TRUE THEN
        RAISE EXCEPTION 'V0133 自检失败：消息中心应可见';
    END IF;
END $$;

-- ⑦ 授权给所有未删除角色（新目录本身）
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, '20000000-0000-0000-0000-000000000880'
  FROM sys_role r
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm
        WHERE rm.role_id = r.id AND rm.menu_id = '20000000-0000-0000-0000-000000000880'
   );
