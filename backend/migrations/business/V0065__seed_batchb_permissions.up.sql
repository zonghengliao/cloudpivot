-- ======================================================
-- V0065：批次 B 数据权限三模块权限点种子（15 个节点 = 3 菜单 + 12 按钮）+ 超管授权
--
-- 依据：`docs/批次B-技术架构.md` §5.1 权限码清单 / 附录 A.6（V0065 原文）
--       + 批次 B · B4 票面（数据范围 API 的权限码与路径）。
-- 回滚：V0065__seed_batchb_permissions.down.sql
--
-- ── 权限码三方一致（本批的硬约束） ──────────────────────────────
-- 每个 `auth_code` 必须同时满足三处逐字一致，任一不一致 = 接口 403：
--   ① `backend/src/business/permission/**/handler.rs` 的 `#[alun::permission]` 注解
--   ② 本迁移写入的 `sys_menu.auth_code`
--   ③ 前端权限常量（`frontend/src/constants/permissions.ts`，B8 落地）
-- 批次 A 的 V0045 头注释记录过同一个坑，这里沿用同样的纪律。
--
-- ── 与架构文档的差异（有意为之，已记入 B4 票面） ────────────────
-- 架构附录 A.6 把数据范围的三个权限码写成 `system:data-scope:{list,update,preview}`，
-- 并把接口挂在 `/system/data-scope/*`。批次 B 重排后，**数据范围的配置接口落在
-- `business/permission/`（本批红线：不改 `system/`）**，票面据此把三个码定为
--   `business:permission:scope:read` / `:write` / `:self-check`
-- 本迁移按**票面**落码（票面是与注解、前端对齐的契约来源）；架构 §5.1 的
-- `system:data-scope:*` 不再单独种入，避免同一能力出现两套码。
-- 字段脱敏 / 角色互斥（B6 / B7）的 9 个码仍严格沿用 §5.1 原文。
--
-- ── 授权策略 ──────────────────────────────────────────────────
-- 只授 `SUPER_ADMIN`（与 `V0020` 只授超管同例）：这三项都是「能改别人能看什么」的
-- 元权限。超管本就由 `role_code` 短路全通，写 `sys_role_menu` 是为了让
-- 「系统管理 → 角色 → 分配菜单」树的勾选状态正确（否则超管在界面上看不到这些节点）。
--
-- ── 菜单 path 与后端 API 前缀无关 ──────────────────────────────
-- `sys_menu.path` 是**前端路由**（`frontend/src/router` 的 path），后端接口在
-- `/api/business/permission/scope/*`。两者不同名不是错误：一个页面可以调多个模块的
-- 接口，路由跟页面走、权限码跟接口走。
--
-- 幂等：固定 UUID + `ON CONFLICT DO NOTHING`；重复执行不报错，也不覆盖运维改名后的标题。
-- ======================================================

-- ---------- 3 个菜单页（menu_type = 2） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000600'::uuid, '数据权限', 2, 'business:permission:scope:read',
     '/system/data-scope', NULL, 'system/data-scope/index',
     60, 1, 'lock-on', '数据权限', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000610'::uuid, '字段脱敏', 2, 'system:field-mask:list',
     '/system/field-mask', NULL, 'system/field-mask/index',
     61, 1, 'mask', '字段脱敏', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000620'::uuid, '角色互斥', 2, 'system:role-mutex:list',
     '/system/role-mutex', NULL, 'system/role-mutex/index',
     62, 1, 'swap', '角色互斥', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 数据范围：3 个按钮（B4 交付，码与注解逐字一致） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000601'::uuid, 'business:permission:scope:read', 3,
     'business:permission:scope:read',
     NULL, '20000000-0000-0000-0000-000000000600'::uuid, NULL,
     10, 1, NULL, '查看数据范围配置 / 自定义部门树', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000602'::uuid, 'business:permission:scope:write', 3,
     'business:permission:scope:write',
     NULL, '20000000-0000-0000-0000-000000000600'::uuid, NULL,
     20, 1, NULL, '修改数据范围配置', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000603'::uuid, 'business:permission:scope:self-check', 3,
     'business:permission:scope:self-check',
     NULL, '20000000-0000-0000-0000-000000000600'::uuid, NULL,
     30, 1, NULL, '数据权限自检（我的范围 / 未接入表）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 字段脱敏：5 个按钮（B6 交付；码取架构 §5.1 原文） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000611'::uuid, 'system:field-mask:list', 3,
     'system:field-mask:list',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL,
     10, 1, NULL, '查看脱敏规则 / 生效清单 / 试运行', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000612'::uuid, 'system:field-mask:create', 3,
     'system:field-mask:create',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL,
     20, 1, NULL, '新建脱敏规则', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000613'::uuid, 'system:field-mask:update', 3,
     'system:field-mask:update',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL,
     30, 1, NULL, '修改脱敏规则', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000614'::uuid, 'system:field-mask:delete', 3,
     'system:field-mask:delete',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL,
     40, 1, NULL, '删除脱敏规则', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000615'::uuid, 'system:field-mask:plain', 3,
     'system:field-mask:plain',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL,
     50, 1, NULL, '查看未脱敏明文（判定位，无独立路由）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 角色互斥：4 个按钮（B7 交付；码取架构 §5.1 原文） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000621'::uuid, 'system:role-mutex:list', 3,
     'system:role-mutex:list',
     NULL, '20000000-0000-0000-0000-000000000620'::uuid, NULL,
     10, 1, NULL, '查看互斥组', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000622'::uuid, 'system:role-mutex:create', 3,
     'system:role-mutex:create',
     NULL, '20000000-0000-0000-0000-000000000620'::uuid, NULL,
     20, 1, NULL, '新建互斥组', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000623'::uuid, 'system:role-mutex:delete', 3,
     'system:role-mutex:delete',
     NULL, '20000000-0000-0000-0000-000000000620'::uuid, NULL,
     30, 1, NULL, '删除互斥组', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000624'::uuid, 'system:role-mutex:conflict', 3,
     'system:role-mutex:conflict',
     NULL, '20000000-0000-0000-0000-000000000620'::uuid, NULL,
     40, 1, NULL, '冲突用户预检 / 用户互斥体检', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 授权：SUPER_ADMIN 全部 15 个节点 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES
    ('20000000-0000-0000-0000-000000000600'::uuid), ('20000000-0000-0000-0000-000000000601'::uuid),
    ('20000000-0000-0000-0000-000000000602'::uuid), ('20000000-0000-0000-0000-000000000603'::uuid),
    ('20000000-0000-0000-0000-000000000610'::uuid), ('20000000-0000-0000-0000-000000000611'::uuid),
    ('20000000-0000-0000-0000-000000000612'::uuid), ('20000000-0000-0000-0000-000000000613'::uuid),
    ('20000000-0000-0000-0000-000000000614'::uuid), ('20000000-0000-0000-0000-000000000615'::uuid),
    ('20000000-0000-0000-0000-000000000620'::uuid), ('20000000-0000-0000-0000-000000000621'::uuid),
    ('20000000-0000-0000-0000-000000000622'::uuid), ('20000000-0000-0000-0000-000000000623'::uuid),
    ('20000000-0000-0000-0000-000000000624'::uuid)
) AS m(id)
WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;
