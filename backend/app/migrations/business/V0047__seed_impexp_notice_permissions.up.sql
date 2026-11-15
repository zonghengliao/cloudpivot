-- ======================================================
-- V0047：导入导出权限点（`business:impexp:*` 13 个 + 3 个菜单页）
--        以及消息通知的**菜单载体补齐**（消息中心 / 发送通知 / 模板页路径修正）
--
-- 依据：`docs/批次A-技术架构.md` §3.8 / §4.8 API 契约、§5.1 权限码清单、§5.3 前端页面清单。
--
-- 与 V0045 的关系（重要）：
--   V0045 已种下 `business:notice:*` 的 9 个权限点，但它的菜单页指向
--   `path='/business/notice'` + `component='business/notice/index'`，与前端实际路由
--   （`/business/notice/template`）不一致 —— 这里用 UPDATE 修正**同一个固定 UUID** 的
--   path/component（不新增重复菜单、不动权限码），并补两个隐藏菜单页：
--     /business/notice/message （is_visible=FALSE，从顶栏铃铛进入，PRD §7.2 口径）
--     /business/notice/send    （is_visible=FALSE，从模板列表「发送通知」进入）
--
-- 授权策略：
--   * `SUPER_ADMIN`   → 全部；
--   * 其它所有角色    → 导入导出主链路 12 个（不含 `export:delete`，删产物只给超管）。
--
-- 幂等：固定 UUID + `ON CONFLICT (id) DO NOTHING`；UPDATE 均为幂等赋值。
-- ======================================================

-- ---------- 修正 V0045 的消息通知菜单路径（同一行，不新增菜单） ----------
UPDATE sys_menu
SET path = '/business/notice/template',
    component = 'business/notice/template/index'
WHERE id = '20000000-0000-0000-0000-000000000410'::uuid;

-- ---------- 导入导出菜单页 3 条 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('22000000-0000-0000-0000-000000000010'::uuid, '导入向导', 2, 'business:impexp:import:upload',
     '/business/impexp/import', '21000000-0000-0000-0000-000000000001'::uuid,
     'business/impexp/import/index',
     30, 1, 'upload', '导入向导', TRUE, 'business'),
    ('22000000-0000-0000-0000-000000000020'::uuid, '导入任务', 2, 'business:impexp:import:list',
     '/business/impexp/import-task', '21000000-0000-0000-0000-000000000001'::uuid,
     'business/impexp/import-task/index',
     40, 1, 'file-import', '导入任务', TRUE, 'business'),
    ('22000000-0000-0000-0000-000000000030'::uuid, '导出任务', 2, 'business:impexp:export:list',
     '/business/impexp/export-task', '21000000-0000-0000-0000-000000000001'::uuid,
     'business/impexp/export-task/index',
     50, 1, 'file-export', '导出任务', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 导入导出按钮权限点 13 条 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('22000000-0000-0000-0000-000000000101'::uuid, 'business:impexp:def:list', 3,
     'business:impexp:def:list', NULL, '22000000-0000-0000-0000-000000000010'::uuid, NULL,
     10, 1, NULL, '导入导出定义查看', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000102'::uuid, 'business:impexp:import:template', 3,
     'business:impexp:import:template', NULL, '22000000-0000-0000-0000-000000000010'::uuid, NULL,
     20, 1, NULL, '下载导入模板', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000103'::uuid, 'business:impexp:import:upload', 3,
     'business:impexp:import:upload', NULL, '22000000-0000-0000-0000-000000000010'::uuid, NULL,
     30, 1, NULL, '上传并建导入任务', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000104'::uuid, 'business:impexp:import:list', 3,
     'business:impexp:import:list', NULL, '22000000-0000-0000-0000-000000000020'::uuid, NULL,
     10, 1, NULL, '导入任务列表 / 详情 / 错误明细', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000105'::uuid, 'business:impexp:import:confirm', 3,
     'business:impexp:import:confirm', NULL, '22000000-0000-0000-0000-000000000020'::uuid, NULL,
     20, 1, NULL, '确认入库', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000106'::uuid, 'business:impexp:import:cancel', 3,
     'business:impexp:import:cancel', NULL, '22000000-0000-0000-0000-000000000020'::uuid, NULL,
     30, 1, NULL, '取消导入任务', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000107'::uuid, 'business:impexp:import:retry', 3,
     'business:impexp:import:retry', NULL, '22000000-0000-0000-0000-000000000020'::uuid, NULL,
     40, 1, NULL, '失败重试', FALSE, 'business'),
    -- A3 后端实际注解是 `import:download`（架构文档写成 download-error），以注解为准
    ('22000000-0000-0000-0000-000000000108'::uuid, 'business:impexp:import:download', 3,
     'business:impexp:import:download', NULL, '22000000-0000-0000-0000-000000000020'::uuid, NULL,
     50, 1, NULL, '下载错误行文件', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000109'::uuid, 'business:impexp:export:create', 3,
     'business:impexp:export:create', NULL, '22000000-0000-0000-0000-000000000030'::uuid, NULL,
     10, 1, NULL, '发起导出 / 同步导出', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000110'::uuid, 'business:impexp:export:list', 3,
     'business:impexp:export:list', NULL, '22000000-0000-0000-0000-000000000030'::uuid, NULL,
     20, 1, NULL, '导出任务列表 / 详情', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000111'::uuid, 'business:impexp:export:download', 3,
     'business:impexp:export:download', NULL, '22000000-0000-0000-0000-000000000030'::uuid, NULL,
     30, 1, NULL, '下载导出结果', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000112'::uuid, 'business:impexp:export:cancel', 3,
     'business:impexp:export:cancel', NULL, '22000000-0000-0000-0000-000000000030'::uuid, NULL,
     40, 1, NULL, '取消导出', FALSE, 'business'),
    ('22000000-0000-0000-0000-000000000113'::uuid, 'business:impexp:export:delete', 3,
     'business:impexp:export:delete', NULL, '22000000-0000-0000-0000-000000000030'::uuid, NULL,
     50, 1, NULL, '删除导出任务与产物', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 消息通知：补两个隐藏菜单页 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('23000000-0000-0000-0000-000000000010'::uuid, '消息中心', 2, 'business:notice:mine',
     '/business/notice/message', '21000000-0000-0000-0000-000000000001'::uuid,
     'business/notice/message/index',
     60, 1, 'notification', '消息中心', FALSE, 'business'),
    ('23000000-0000-0000-0000-000000000020'::uuid, '发送通知', 2, 'business:notice:send',
     '/business/notice/send', '20000000-0000-0000-0000-000000000410'::uuid,
     'business/notice/send/index',
     95, 1, 'send', '发送通知', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 授权：SUPER_ADMIN 全部 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES
    ('22000000-0000-0000-0000-000000000010'::uuid),
    ('22000000-0000-0000-0000-000000000020'::uuid),
    ('22000000-0000-0000-0000-000000000030'::uuid),
    ('22000000-0000-0000-0000-000000000101'::uuid),
    ('22000000-0000-0000-0000-000000000102'::uuid),
    ('22000000-0000-0000-0000-000000000103'::uuid),
    ('22000000-0000-0000-0000-000000000104'::uuid),
    ('22000000-0000-0000-0000-000000000105'::uuid),
    ('22000000-0000-0000-0000-000000000106'::uuid),
    ('22000000-0000-0000-0000-000000000107'::uuid),
    ('22000000-0000-0000-0000-000000000108'::uuid),
    ('22000000-0000-0000-0000-000000000109'::uuid),
    ('22000000-0000-0000-0000-000000000110'::uuid),
    ('22000000-0000-0000-0000-000000000111'::uuid),
    ('22000000-0000-0000-0000-000000000112'::uuid),
    ('22000000-0000-0000-0000-000000000113'::uuid),
    ('23000000-0000-0000-0000-000000000010'::uuid),
    ('23000000-0000-0000-0000-000000000020'::uuid)
) AS m(id)
WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 授权：其余所有角色 → 导入导出主链路 12 个 + 消息中心菜单页 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES
    ('22000000-0000-0000-0000-000000000010'::uuid),
    ('22000000-0000-0000-0000-000000000020'::uuid),
    ('22000000-0000-0000-0000-000000000030'::uuid),
    ('22000000-0000-0000-0000-000000000101'::uuid),
    ('22000000-0000-0000-0000-000000000102'::uuid),
    ('22000000-0000-0000-0000-000000000103'::uuid),
    ('22000000-0000-0000-0000-000000000104'::uuid),
    ('22000000-0000-0000-0000-000000000105'::uuid),
    ('22000000-0000-0000-0000-000000000106'::uuid),
    ('22000000-0000-0000-0000-000000000107'::uuid),
    ('22000000-0000-0000-0000-000000000108'::uuid),
    ('22000000-0000-0000-0000-000000000109'::uuid),
    ('22000000-0000-0000-0000-000000000110'::uuid),
    ('22000000-0000-0000-0000-000000000111'::uuid),
    ('22000000-0000-0000-0000-000000000112'::uuid),
    ('23000000-0000-0000-0000-000000000010'::uuid)
) AS m(id)
WHERE r.role_code <> 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;
