-- ======================================================
-- V0046：对象存储权限点种子（`business:storage:*` 11 个 + 菜单载体 + 一级目录）
--
-- 依据：`docs/批次A-技术架构.md` §2.9 API 契约 / §5.1 权限码清单 / §5.3 前端页面清单。
-- 权限码与 `backend/src/business/storage/handler.rs` 的 `#[nik::permission]` **逐字一致**，
-- 也与前端 `frontend/src/constants/permissions.ts` 的 `STORAGE_PERMISSIONS` 一致（三方同源）。
--
-- 授权策略（§5.1 注）：
--   * `SUPER_ADMIN`            → 11 个全给（超管由 role_code 短路本就全通，
--                                 写 sys_role_menu 只是让「角色授权」树的勾选状态正确）；
--   * 其它**所有角色**        → 只给业务主链路 3 个：
--                                 `business:storage:upload` / `:list` / `:download`
--                                 （任何角色都可能要传附件、看自己的文件、下载）；
--                                 **配置管理、策略、删除仍只给超管**。
--
-- 菜单路径与前端路由逐字对应（`frontend/src/router/index.ts`），不一致会导致侧边栏点不开：
--   /business/storage/backend → views/business/storage/backend/index.vue
--   /business/storage/file    → views/business/storage/file/index.vue
--
-- 幂等：固定 UUID + `ON CONFLICT (id) DO NOTHING`；重复执行不报错，也不覆盖运维改名后的标题。
-- ======================================================

-- ---------- 一级目录「通用底座」（sort 70，PRD §7.1） ----------
-- 注意：`sys_menu.auth_code` 是 NOT NULL，目录节点沿用底座种子的写法填空串 ''（不是 NULL），
-- 否则插入直接违反非空约束（本地 PG 18.6 实测）。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES ('21000000-0000-0000-0000-000000000001'::uuid, '通用底座', 1, '',
        '/business', NULL, NULL,
        70, 1, 'app', '通用底座', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 菜单页 2 条 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('21000000-0000-0000-0000-000000000010'::uuid, '存储管理', 2, 'business:storage:config:list',
     '/business/storage/backend', '21000000-0000-0000-0000-000000000001'::uuid,
     'business/storage/backend/index',
     10, 1, 'object-storage', '存储管理', TRUE, 'business'),
    ('21000000-0000-0000-0000-000000000020'::uuid, '文件管理', 2, 'business:storage:list',
     '/business/storage/file', '21000000-0000-0000-0000-000000000001'::uuid,
     'business/storage/file/index',
     20, 1, 'folder', '文件管理', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 按钮权限点 11 条（menu_type=3） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('21000000-0000-0000-0000-000000000101'::uuid, 'business:storage:config:list', 3,
     'business:storage:config:list', NULL, '21000000-0000-0000-0000-000000000010'::uuid, NULL,
     10, 1, NULL, '存储配置查看', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000102'::uuid, 'business:storage:config:create', 3,
     'business:storage:config:create', NULL, '21000000-0000-0000-0000-000000000010'::uuid, NULL,
     20, 1, NULL, '新增存储配置', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000103'::uuid, 'business:storage:config:update', 3,
     'business:storage:config:update', NULL, '21000000-0000-0000-0000-000000000010'::uuid, NULL,
     30, 1, NULL, '编辑 / 设默认 / 启停存储配置', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000104'::uuid, 'business:storage:config:delete', 3,
     'business:storage:config:delete', NULL, '21000000-0000-0000-0000-000000000010'::uuid, NULL,
     40, 1, NULL, '删除存储配置', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000105'::uuid, 'business:storage:config:test', 3,
     'business:storage:config:test', NULL, '21000000-0000-0000-0000-000000000010'::uuid, NULL,
     50, 1, NULL, '连通性测试', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000106'::uuid, 'business:storage:policy:list', 3,
     'business:storage:policy:list', NULL, '21000000-0000-0000-0000-000000000010'::uuid, NULL,
     60, 1, NULL, '上传策略查看', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000107'::uuid, 'business:storage:policy:update', 3,
     'business:storage:policy:update', NULL, '21000000-0000-0000-0000-000000000010'::uuid, NULL,
     70, 1, NULL, '上传策略编辑', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000111'::uuid, 'business:storage:upload', 3,
     'business:storage:upload', NULL, '21000000-0000-0000-0000-000000000020'::uuid, NULL,
     10, 1, NULL, '上传文件（含直传）', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000112'::uuid, 'business:storage:list', 3,
     'business:storage:list', NULL, '21000000-0000-0000-0000-000000000020'::uuid, NULL,
     20, 1, NULL, '文件分页查看', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000113'::uuid, 'business:storage:download', 3,
     'business:storage:download', NULL, '21000000-0000-0000-0000-000000000020'::uuid, NULL,
     30, 1, NULL, '文件下载 / 限时链接', FALSE, 'business'),
    ('21000000-0000-0000-0000-000000000114'::uuid, 'business:storage:delete', 3,
     'business:storage:delete', NULL, '21000000-0000-0000-0000-000000000020'::uuid, NULL,
     40, 1, NULL, '删除文件（软删）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 授权：SUPER_ADMIN 全部（目录 + 2 菜单 + 11 按钮） ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES
    ('21000000-0000-0000-0000-000000000001'::uuid),
    ('21000000-0000-0000-0000-000000000010'::uuid),
    ('21000000-0000-0000-0000-000000000020'::uuid),
    ('21000000-0000-0000-0000-000000000101'::uuid),
    ('21000000-0000-0000-0000-000000000102'::uuid),
    ('21000000-0000-0000-0000-000000000103'::uuid),
    ('21000000-0000-0000-0000-000000000104'::uuid),
    ('21000000-0000-0000-0000-000000000105'::uuid),
    ('21000000-0000-0000-0000-000000000106'::uuid),
    ('21000000-0000-0000-0000-000000000107'::uuid),
    ('21000000-0000-0000-0000-000000000111'::uuid),
    ('21000000-0000-0000-0000-000000000112'::uuid),
    ('21000000-0000-0000-0000-000000000113'::uuid),
    ('21000000-0000-0000-0000-000000000114'::uuid)
) AS m(id)
WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 授权：其余所有角色 → 目录 + 文件管理页 + 上传/列表/下载 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES
    ('21000000-0000-0000-0000-000000000001'::uuid),
    ('21000000-0000-0000-0000-000000000020'::uuid),
    ('21000000-0000-0000-0000-000000000111'::uuid),
    ('21000000-0000-0000-0000-000000000112'::uuid),
    ('21000000-0000-0000-0000-000000000113'::uuid)
) AS m(id)
WHERE r.role_code <> 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;
