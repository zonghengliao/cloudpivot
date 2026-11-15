-- ============================================================================
-- V0322：首次启动向导（FIX-1）的两个权限码 —— 让「指派审批人」授得出去
--
-- ## 为什么必须带权限迁移
--
-- `scripts/verify/perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：
-- `#[nik::permission(permission = "x")]` 里的每个码都必须在 `sys_menu.auth_code`
-- 有承载行，否则该码**授不出去** ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，
-- 而且不报错、不打日志。`system/setup/handler.rs` 的三个接口用两个码：
--
--   system:setup:read     GET  /system/setup/check        关键角色谁还没人
--   system:setup:manage   POST /system/setup/assign       批量指派角色成员
--                         POST /system/setup/demo-users   一键建演示账号
--
-- ## 结构：系统管理目录下的两个按钮（menu_type=3）
--
--   system（目录 10000000-…-0001）
--     ├─ system:setup:read    （id …3401，隐藏按钮）
--     └─ system:setup:manage  （id …3402，隐藏按钮）
--
-- 两个都是 `menu_type = 3`（按钮）→ 不参与侧边栏与 `system::menu::tests::n08`
-- 的路由解析门禁（那条门禁只认 `menu_type = 2` 的 path）。
--
-- 前端向导是**登录后的弹层**（`BasicLayout` 里挂 `SetupWizardDialog`），
-- 不是独立路由页 —— 所以这里不种 `menu_type = 2` 的页面节点。
-- 若要改成独立页面，追加一条迁移把 read 那条改成 `menu_type = 2` 并补 path/component。
--
-- ## 角色授权
--
-- **不给任何角色种 `sys_role_menu`**：超管拿 `*:*:*` 通配（对功能零影响），
-- 其余角色要不要具备「给审批岗加人」的能力是运维决定，交「角色管理 → 分配菜单权限」。
-- 与 V0310 的做法一致（那里也不给超管种行）。
--
-- ## 幂等 / 回滚
--
-- 固定 uuid + `ON CONFLICT (id) DO UPDATE`（改名 / 改标题可自愈，与 S__menu_data.sql 同口径）；
-- 回滚：V0322__setup_wizard_permissions.down.sql 按 id 删除两行。
-- ============================================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003401'::uuid, 'system:setup:read', 3,
     'system:setup:read', NULL, '10000000-0000-0000-0000-000000000001'::uuid, NULL,
     90, 1, NULL, '首次启动向导（查看）', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000003402'::uuid, 'system:setup:manage', 3,
     'system:setup:manage', NULL, '10000000-0000-0000-0000-000000000001'::uuid, NULL,
     91, 1, NULL, '首次启动向导（指派审批人）', FALSE, 'system')
ON CONFLICT (id) DO UPDATE
   SET name       = EXCLUDED.name,
       menu_type  = EXCLUDED.menu_type,
       auth_code  = EXCLUDED.auth_code,
       parent_id  = EXCLUDED.parent_id,
       sort_order = EXCLUDED.sort_order,
       status     = EXCLUDED.status,
       title      = EXCLUDED.title,
       is_visible = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       updated_at = now();

-- 自检：两个码都必须在（迁移失败要比"静默死权限"早暴露）
DO $$
DECLARE
    n_read   int;
    n_manage int;
BEGIN
    SELECT count(*) INTO n_read FROM sys_menu
     WHERE auth_code = 'system:setup:read' AND deleted_at IS NULL;
    SELECT count(*) INTO n_manage FROM sys_menu
     WHERE auth_code = 'system:setup:manage' AND deleted_at IS NULL;
    IF n_read = 0 THEN
        RAISE EXCEPTION 'V0322 自检失败：system:setup:read 未种';
    END IF;
    IF n_manage = 0 THEN
        RAISE EXCEPTION 'V0322 自检失败：system:setup:manage 未种';
    END IF;
END $$;
