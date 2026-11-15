-- ======================================================
-- V0088：帮助文档权限种子（批次 C · C4）+ 全员读入口
--
-- 依据：docs/批次C-技术架构.md §3.4（权限：全员可读 / 管理码授超管）、
--       §5.1（权限码 → 接口映射）、§5.5.2 H-14（无 system:help:read 不存在）
-- 回滚：V0088__seed_help_permissions.down.sql
--
-- ── 为什么是 V0088（号段说明）────────────────────────────────
-- 架构 §5.3 把批次 C 的权限种子统一排在 V0085 / V0086，但那是"整个批次一次性
-- 落"的理想排期；本批是**并卡交付**：C2 已用 V0087 补了 `business:announcement:read`，
-- C3（公告管理侧）/ C6（变更日志运维）还会各占号。为了避免两卡同时写 V0085 造成
-- 「迁移版本重复 → 启动即失败」，C4 按"单用途种子迁移"的既有范式取预留带末尾的
-- **V0088**。将来 V0085 若真的一次性补齐 20 码，对本迁移种的节点是
-- `ON CONFLICT (id) DO NOTHING` 幂等，不冲突。
--
-- ── 菜单 UUID 段：...0740 ~ ...0750 ────────────────────────
-- C2 占 ...0700/0701（公告用户侧），C3 管理侧从 ...0710 起，变更日志 ...0730；
-- 帮助文档整体排在 **0740 起**，与三者都不撞号。
--
-- ── 授权策略（架构 §3.4 的结论）────────────────────────────
--   · `system:help:read`：**所有未软删角色**都授。帮助文档是"产品自带说明书"，
--     按角色开放会导致新员工入职第一天看不到任何文档。
--   · 其余 9 个管理码：只授 `SUPER_ADMIN`（编辑权限是元权限；内容运营角色由
--     运维按需追加授权，迁移不替运维做角色设计）。
--
-- ── 幂等 ──────────────────────────────────────────────────
-- 固定 UUID + `ON CONFLICT DO NOTHING`：重复执行不报错，也不覆盖运维改过的标题。
-- ======================================================

-- ---------- 1) 菜单与按钮节点 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    -- 帮助中心页面（全员可见的入口）
    ('20000000-0000-0000-0000-000000000740'::uuid, '帮助中心', 2, 'system:help:read',
     '/help', NULL, 'business/help/index',
     74, 1, 'question-circle', '帮助中心', TRUE, 'business'),
    -- 用户侧读按钮（列表 / 详情 / 检索 / 阅读量）
    ('20000000-0000-0000-0000-000000000741'::uuid, 'system:help:read', 3,
     'system:help:read',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     10, 1, NULL, '查看帮助文档（分类树 / 列表 / 详情 / 检索）', FALSE, 'business'),
    -- 分类管理
    ('20000000-0000-0000-0000-000000000742'::uuid, 'system:help:category:list', 3,
     'system:help:category:list',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     20, 1, NULL, '分类管理列表（含停用）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000743'::uuid, 'system:help:category:create', 3,
     'system:help:category:create',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     30, 1, NULL, '新建分类', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000744'::uuid, 'system:help:category:update', 3,
     'system:help:category:update',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     40, 1, NULL, '编辑 / 移动分类', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000745'::uuid, 'system:help:category:delete', 3,
     'system:help:category:delete',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     50, 1, NULL, '删除分类', FALSE, 'business'),
    -- 文档管理
    ('20000000-0000-0000-0000-000000000746'::uuid, 'system:help:doc:list', 3,
     'system:help:doc:list',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     60, 1, NULL, '文档管理列表 / 编辑回显 / 版本列表', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000747'::uuid, 'system:help:doc:create', 3,
     'system:help:doc:create',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     70, 1, NULL, '新建文档（草稿）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000748'::uuid, 'system:help:doc:update', 3,
     'system:help:doc:update',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     80, 1, NULL, '编辑文档', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000749'::uuid, 'system:help:doc:delete', 3,
     'system:help:doc:delete',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     90, 1, NULL, '删除文档（软删）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000750'::uuid, 'system:help:doc:publish', 3,
     'system:help:doc:publish',
     NULL, '20000000-0000-0000-0000-000000000740'::uuid, NULL,
     100, 1, NULL, '发布 / 回滚文档', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 2) 全员读入口（页面 + 读按钮） ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000740'::uuid),
                    ('20000000-0000-0000-0000-000000000741'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 3) 管理码只授超管 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000742'::uuid),
                    ('20000000-0000-0000-0000-000000000743'::uuid),
                    ('20000000-0000-0000-0000-000000000744'::uuid),
                    ('20000000-0000-0000-0000-000000000745'::uuid),
                    ('20000000-0000-0000-0000-000000000746'::uuid),
                    ('20000000-0000-0000-0000-000000000747'::uuid),
                    ('20000000-0000-0000-0000-000000000748'::uuid),
                    ('20000000-0000-0000-0000-000000000749'::uuid),
                    ('20000000-0000-0000-0000-000000000750'::uuid)) AS m(id)
 WHERE r.role_code = 'SUPER_ADMIN'
   AND r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 4) 自检（失败即迁移失败） ----------
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000000740'::uuid;
    read_menu uuid := '20000000-0000-0000-0000-000000000741'::uuid;
    total     int;
    missing   int;
BEGIN
    SELECT count(*) INTO total
      FROM sys_menu
     WHERE id BETWEEN '20000000-0000-0000-0000-000000000740'::uuid
                  AND '20000000-0000-0000-0000-000000000750'::uuid
       AND deleted_at IS NULL;
    IF total <> 11 THEN
        RAISE EXCEPTION 'V0088 自检失败：帮助文档菜单节点应有 11 个，实际 %', total;
    END IF;

    -- "无 system:help:read 的角色" 必须不存在（H-14）
    SELECT count(*) INTO missing
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = r.id AND rm.menu_id = read_menu
           );
    IF missing > 0 THEN
        RAISE EXCEPTION 'V0088 自检失败：还有 % 个未软删角色没有 system:help:read 授权', missing;
    END IF;

    -- 页面节点必须在
    IF NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = page_menu AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0088 自检失败：帮助中心菜单节点 % 不存在', page_menu;
    END IF;
END
$$;
