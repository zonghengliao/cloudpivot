-- ======================================================
-- V0020：新增权限点 business:monitor:server-status（服务器状态，仅超管）
--
-- 背景：工作台右栏需展示服务器运行状态（CPU/内存/磁盘/DB/Redis），
--       该信息只对超级管理员开放；接口由
--       src/business/monitor/handler.rs 的 #[alun::permission] 校验，
--       权限码必须与 sys_menu.auth_code 逐字一致。
--
-- 权限码：business:monitor:server-status
-- 授权范围：仅 SUPER_ADMIN。
--   超管鉴权本身由 role_code 短路（src/system/auth/model.rs::is_super_admin，
--   权限列表下发 *:*:*），此处写入 sys_role_menu 是为了让「角色授权」页的
--   权限树能完整展示本权限点，并非鉴权所必需。
--
-- 幂等：固定 UUID + ON CONFLICT DO NOTHING，重复执行不报错、不覆盖人工改名。
-- ======================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES ('20000000-0000-0000-0000-000000000001'::uuid,
        'business:monitor:server-status', 3, 'business:monitor:server-status',
        NULL, '10000000-0000-0000-0000-000000000050'::uuid, NULL,
        20, 1, NULL, '服务器状态', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000001'::uuid)) AS m(id)
WHERE r.role_code = 'SUPER_ADMIN'
ON CONFLICT DO NOTHING;
