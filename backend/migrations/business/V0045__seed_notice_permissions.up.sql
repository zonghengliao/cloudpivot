-- ======================================================
-- V0045：消息通知权限点种子（`business:notice:*` 9 个 + 菜单载体）
--
-- 依据：`docs/批次A-技术架构.md` §4.8 API 契约与 §5.1 权限码清单。
-- 权限码必须与 `src/business/notice/handler.rs` 的 `#[alun::permission]` **逐字一致**，
-- 否则接口 403；改这里必须同步改 Rust 声明（反之亦然）。
--
-- 授权策略（与 §5.1 的注一致）：
--   * `SUPER_ADMIN`   → 全部 9 个权限点（超管由 role_code 短路本就全通，
--                       写 sys_role_menu 只是让「角色授权」树的勾选状态正确）；
--   * 其它**所有角色** → 仅 `business:notice:mine`（我的通知 / 未读数 / 已读 / 删除我的）。
--     消息中心是「人人要用」的页面，底座所有 handler 都带权限宏、没有「登录即可访问」的
--     豁免口子，不授予普通角色会让非超管进页面直接 403。
--     新建角色时需在「系统管理 → 角色 → 分配菜单」里勾选该项。
--
-- 幂等：固定 UUID + 固定权限码，`ON CONFLICT (id) DO NOTHING`；重复执行不报错，
-- 也不会覆盖运维改名后的标题。
-- ======================================================

-- ---------- 菜单与按钮（menu_type：1 目录 / 2 菜单 / 3 按钮） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000410'::uuid, '消息通知', 2, 'business:notice:list',
     '/business/notice', NULL, 'business/notice/index',
     40, 1, 'notification', '消息通知', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000411'::uuid, 'business:notice:send', 3, 'business:notice:send',
     NULL, '20000000-0000-0000-0000-000000000410'::uuid, NULL,
     10, 1, NULL, '发送通知', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000412'::uuid, 'business:notice:list', 3, 'business:notice:list',
     NULL, '20000000-0000-0000-0000-000000000410'::uuid, NULL,
     20, 1, NULL, '通知列表 / 详情', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000413'::uuid, 'business:notice:delete', 3, 'business:notice:delete',
     NULL, '20000000-0000-0000-0000-000000000410'::uuid, NULL,
     30, 1, NULL, '删除通知', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000414'::uuid, 'business:notice:mine', 3, 'business:notice:mine',
     NULL, '20000000-0000-0000-0000-000000000410'::uuid, NULL,
     40, 1, NULL, '我的通知 / 未读数 / 已读', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000415'::uuid, 'business:notice:template:list', 3,
     'business:notice:template:list',
     NULL, '20000000-0000-0000-0000-000000000410'::uuid, NULL,
     50, 1, NULL, '模板列表 / 预览', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000416'::uuid, 'business:notice:template:create', 3,
     'business:notice:template:create',
     NULL, '20000000-0000-0000-0000-000000000410'::uuid, NULL,
     60, 1, NULL, '新建模板', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000417'::uuid, 'business:notice:template:update', 3,
     'business:notice:template:update',
     NULL, '20000000-0000-0000-0000-000000000410'::uuid, NULL,
     70, 1, NULL, '修改模板', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000418'::uuid, 'business:notice:template:delete', 3,
     'business:notice:template:delete',
     NULL, '20000000-0000-0000-0000-000000000410'::uuid, NULL,
     80, 1, NULL, '删除模板', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000419'::uuid, 'business:notice:channel:config', 3,
     'business:notice:channel:config',
     NULL, '20000000-0000-0000-0000-000000000410'::uuid, NULL,
     90, 1, NULL, '通道配置查看', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 授权：SUPER_ADMIN 全部 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES
    ('20000000-0000-0000-0000-000000000410'::uuid),
    ('20000000-0000-0000-0000-000000000411'::uuid),
    ('20000000-0000-0000-0000-000000000412'::uuid),
    ('20000000-0000-0000-0000-000000000413'::uuid),
    ('20000000-0000-0000-0000-000000000414'::uuid),
    ('20000000-0000-0000-0000-000000000415'::uuid),
    ('20000000-0000-0000-0000-000000000416'::uuid),
    ('20000000-0000-0000-0000-000000000417'::uuid),
    ('20000000-0000-0000-0000-000000000418'::uuid),
    ('20000000-0000-0000-0000-000000000419'::uuid)
) AS m(id)
WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 授权：其余所有角色仅「我的通知」（消息中心对全员开放） ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, '20000000-0000-0000-0000-000000000414'::uuid, CURRENT_TIMESTAMP
FROM sys_role r
WHERE r.role_code <> 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;
