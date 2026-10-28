-- ======================================================
-- V0052：补种 `business:impexp:export:retry` 权限点（批次 A 遗留修复 · 项⑤）
--
-- 问题（A7 前端联调时暴露，但当时 A4 未合并所以没被发现）：
--   A4 的 handler 声明了 `POST /business/impexp/export/{id}/retry`
--   → 权限码 `business:impexp:export:retry`，但 V0047 只种了 13 个 impexp 权限点，
--   **不含 `export:retry`**。非超管角色因此永远 403：按钮可见（前端按同一码判定），
--   点下去必然失败 —— 「导出重试」对普通运维账号是废功能。
--
-- 本迁移只做三件事（都是幂等 INSERT，不改既有行）：
--   1) 在「导出任务」菜单页下补一个按钮级权限点（menu_type=3）；
--   2) 授予 SUPER_ADMIN；
--   3) 授予其余所有角色（与 V0047「其余角色 → 主链路」口径一致，
--      `export:delete` 仍只给超管，重试属于主链路能力）。
--
-- 迁移号段：V0040–V0051 已用，本卡从 V0051 起，故取 V0052。
-- ======================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('22000000-0000-0000-0000-000000000114'::uuid, 'business:impexp:export:retry', 3,
     'business:impexp:export:retry', NULL, '22000000-0000-0000-0000-000000000030'::uuid, NULL,
     45, 1, NULL, '重试导出任务', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 授权：SUPER_ADMIN ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, '22000000-0000-0000-0000-000000000114'::uuid, CURRENT_TIMESTAMP
FROM sys_role r
WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 授权：其余所有角色（主链路能力） ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, '22000000-0000-0000-0000-000000000114'::uuid, CURRENT_TIMESTAMP
FROM sys_role r
WHERE r.role_code <> 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;
