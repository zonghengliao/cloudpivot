-- ============================================================
-- V0349：附件必传规则管理界面（`/support/attachment-rule`）菜单 + 权限点 —— **ATT-09**
--
-- # 这条迁移补的是什么
--
-- ATT-06 建了 `biz_attachment_rule`（9 条预置规则）与闸门，ATT-09 给它补管理面：
-- 页面 `/support/attachment-rule` + 4 个权限点（list / create / update / delete）。
-- 规则表本身**不改**（V0347 的 DDL 一个字都不动），本迁移只碰 `sys_menu` /
-- `sys_role_menu`。
--
-- # 为什么权限点只授给 SUPER_ADMIN
--
-- 规则决定「哪些单据在哪个动作提交不了」，属于**内控配置**。后端还有第二层
-- `dto::ensure_super_admin`（角色 SUPER_ADMIN 或通配权限），所以即使有人日后在
-- 「角色授权」页把码勾给别的角色，写接口仍然 403 —— 这里是纵深防御，不是唯一防线。
-- 卡面验收⑤「非超管账号打开 → 403」由第一层（`support:attachment-rule:list`
-- 只授超管）+ 第二层（`ensure_super_admin`）共同保证。
--
-- # 菜单口径（AGENTS.md 约定 12）
--
--   name   = `support:attachment-rule`（唯一标识，与权限码前缀一致）
--   title  = `附件规则`（侧边栏显示名，4 字，不带标点 / 说明）
--   path   = `/support/attachment-rule`（与前端路由、`component` 逐字对齐）
--   parent = `10000000-0000-0000-0000-000000000022`（支撑功能目录）
--   sort   = 80（支撑功能下已有 10/20/30/40/50/60 + 账务巡检 70，**同父唯一**；
--            首版写 70，被 `scripts/verify/menu_invariants.sh` ⑦ 断言拦下后改 80）
--
-- # 幂等
--
--   固定 UUID + `ON CONFLICT (id) DO NOTHING`：重复执行不报错，也不覆盖人工改名
--   （沿用 V0343 的口径）。`sys_role_menu` 用 `ON CONFLICT DO NOTHING`。
--   ⚠ 空库首启时 `SUPER_ADMIN` 还没种（seed 在 V* 之后跑）→ 授权 0 行，
--   与 V0020 / V0343 同口径；鉴权短路不依赖这张表，权限树在升级库上可见。
--
-- # 回滚
--
--   V0349.down.sql：删 4 条 `sys_role_menu` + 4 条 `sys_menu`（按固定 UUID，
--   不按 auth_code 通配删 —— 避免误伤同名权限点）。
-- ============================================================

-- ============================================================
-- §1 页面菜单（menu_type = 2）
-- ============================================================
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES ('20000000-0000-0000-0000-0000000a0901'::uuid,
        'support:attachment-rule', 2, 'support:attachment-rule:list',
        '/support/attachment-rule', '10000000-0000-0000-0000-000000000022'::uuid,
        'support/attachment-rule/index',
        80, 1, 'file', '附件规则', TRUE, 'support')
ON CONFLICT (id) DO NOTHING;

-- ============================================================
-- §2 按钮 / 权限点（menu_type = 3，同父 sort 10/20/30 唯一）
-- ============================================================
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES ('20000000-0000-0000-0000-0000000a0902'::uuid,
        'support:attachment-rule:create', 3, 'support:attachment-rule:create',
        NULL, '20000000-0000-0000-0000-0000000a0901'::uuid, NULL,
        10, 1, NULL, '新增规则', FALSE, 'support'),
       ('20000000-0000-0000-0000-0000000a0903'::uuid,
        'support:attachment-rule:update', 3, 'support:attachment-rule:update',
        NULL, '20000000-0000-0000-0000-0000000a0901'::uuid, NULL,
        20, 1, NULL, '编辑规则', FALSE, 'support'),
       ('20000000-0000-0000-0000-0000000a0904'::uuid,
        'support:attachment-rule:delete', 3, 'support:attachment-rule:delete',
        NULL, '20000000-0000-0000-0000-0000000a0901'::uuid, NULL,
        30, 1, NULL, '删除规则', FALSE, 'support')
ON CONFLICT (id) DO NOTHING;

-- ============================================================
-- §3 授权给 SUPER_ADMIN（空库首启 0 行，见文件头）
-- ============================================================
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES ('20000000-0000-0000-0000-0000000a0901'::uuid),
                   ('20000000-0000-0000-0000-0000000a0902'::uuid),
                   ('20000000-0000-0000-0000-0000000a0903'::uuid),
                   ('20000000-0000-0000-0000-0000000a0904'::uuid)) AS m(id)
WHERE r.role_code = 'SUPER_ADMIN'
ON CONFLICT DO NOTHING;
