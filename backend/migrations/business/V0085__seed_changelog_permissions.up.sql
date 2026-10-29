-- ======================================================
-- V0085：批次 C 权限种子（本卡补齐「变更日志」+「公告管理」两段）
--
-- 依据：docs/批次C-技术架构.md
--   §5.1.1 公告管理码 6 个（`business:announcement:{list,create,update,delete,publish,offline}`）
--   §5.1.3 变更日志码 3 个（`system:change-log:{list,export,clean}`）
--   §5.1.4 菜单 UUID 段（…0710 公告管理 / …0711–0716 六个按钮
--                        …0730 变更日志 / …0731–0733 三个按钮）
--   §5.3   V0085 = 权限种子（架构原计划一次性播 20 码 + 4 菜单）
--   §7 C8 验收：新权限码在「角色-菜单」树可见；非超管能进公告中心/帮助中心、进不了变更日志
-- 回滚：V0085__seed_changelog_permissions.down.sql
--
-- ── 为什么本文件叫「changelog_permissions」却还带了公告管理码 ─────────────
-- 架构 §5.3 的 20 码要「一次性」落在 V0085，但本批是**并卡交付**，号段被拆用了：
--   · V0087（C2）已种 `business:announcement:read` + 公告中心 …0700/…0701；
--   · V0088（C4）已种 `system:help:*` 10 码 + 帮助中心 …0740–…0750。
-- C8 是收尾卡，剩下的缺口正好是两块：公告**管理侧** 6 码、变更日志 3 码。
-- 两者必须同一迁移：`business:announcement:list` 既是公告管理页的 `auth_code`
-- 又是 6 个按钮之首，拆两个迁移会出现「先建按钮、父菜单还不存在」的中间态。
-- 文件名沿用任务书的交付约定（`V0085__seed_changelog_permissions`），内容含两段。
--
-- ── 已存在的部分（本迁移不重复种，只做自检）────────────────────────────
--   `business:announcement:read`  → V0087（…0700 / …0701）
--   `system:help:*`（10 码）       → V0088（…0740 / …0741–…0750）
-- 本迁移对 …0700/…0740 段**不写任何行**，只断言它们仍在，避免下游误以为帮助/公告
-- 用户侧入口没种。
--
-- ── 授权策略（架构 §5.1.4 的三段结构）────────────────────────────────
--   ① SUPER_ADMIN：本迁移的 11 个节点全给；
--   ② 其余角色：**一个都不给** —— 公告管理属元权限（谁发公告谁负责），
--      「公告管理员」这类角色由运维按需追加授权，迁移不替运维做角色设计；
--   ③ `AUDITOR`（若运维已建该角色）：只给变更日志页 + list/export 两按钮，
--      **不给 clean**（架构 §5.1.3：手工清理过期分区仅 SUPER_ADMIN）。
--   验收要求「非超管进不了变更日志」，因此末尾自检会断言：
--   除 SUPER_ADMIN / AUDITOR 以外的角色，持有变更日志节点的授权数必须为 0。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
-- 固定 UUID + `ON CONFLICT (id) DO NOTHING`：重复执行不报错，也不覆盖运维改过的
-- 标题 / 排序 / 图标（种子只负责「存在」，不负责「纠正」）。
-- ======================================================

-- ---------- 1) 公告管理菜单（…0710）+ 6 个按钮（…0711–…0716） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    -- 页面：公告管理（管理侧列表 / 详情 / 阅读统计的入口）
    -- component 用 `business/announcement/admin`：…0700 已被 C2 的「公告中心」占用
    -- `business/announcement/index`，两个菜单指向同一组件会让前端路由只能解析到一个页面。
    -- C9 若最终选了别的文件名，改这一行即可（单点，不动其它节点）。
    ('20000000-0000-0000-0000-000000000710'::uuid, '公告管理', 2, 'business:announcement:list',
     '/business/announcement', NULL, 'business/announcement/admin',
     71, 1, 'announcement', '公告管理', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000711'::uuid, 'business:announcement:list', 3,
     'business:announcement:list',
     NULL, '20000000-0000-0000-0000-000000000710'::uuid, NULL,
     10, 1, NULL, '管理列表 / 详情 / 阅读统计 / 预览', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000712'::uuid, 'business:announcement:create', 3,
     'business:announcement:create',
     NULL, '20000000-0000-0000-0000-000000000710'::uuid, NULL,
     20, 1, NULL, '新建草稿', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000713'::uuid, 'business:announcement:update', 3,
     'business:announcement:update',
     NULL, '20000000-0000-0000-0000-000000000710'::uuid, NULL,
     30, 1, NULL, '编辑 / 置顶 / 取消置顶', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000714'::uuid, 'business:announcement:delete', 3,
     'business:announcement:delete',
     NULL, '20000000-0000-0000-0000-000000000710'::uuid, NULL,
     40, 1, NULL, '删除（软删）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000715'::uuid, 'business:announcement:publish', 3,
     'business:announcement:publish',
     NULL, '20000000-0000-0000-0000-000000000710'::uuid, NULL,
     50, 1, NULL, '发布（含扇出）/ 再次发布', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000716'::uuid, 'business:announcement:offline', 3,
     'business:announcement:offline',
     NULL, '20000000-0000-0000-0000-000000000710'::uuid, NULL,
     60, 1, NULL, '下线', FALSE, 'business'),
    -- 页面：变更日志（…0730）+ 3 个按钮（…0731–…0733）
    -- component 按架构 §5.1.4 用 `system/change-log/index`（代码在 business/changelog，
    -- 权限码按任务书用 system:change-log:*，与批次 B 的 business/masking 用
    -- system:field-mask:* 是同一先例）。
    ('20000000-0000-0000-0000-000000000730'::uuid, '变更日志', 2, 'system:change-log:list',
     '/system/change-log', NULL, 'system/change-log/index',
     73, 1, 'history', '变更日志', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000731'::uuid, 'system:change-log:list', 3,
     'system:change-log:list',
     NULL, '20000000-0000-0000-0000-000000000730'::uuid, NULL,
     10, 1, NULL, '全局检索 / 详情 / 对象历史 / 分区清单', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000732'::uuid, 'system:change-log:export', 3,
     'system:change-log:export',
     NULL, '20000000-0000-0000-0000-000000000730'::uuid, NULL,
     20, 1, NULL, '导出检索结果', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000733'::uuid, 'system:change-log:clean', 3,
     'system:change-log:clean',
     NULL, '20000000-0000-0000-0000-000000000730'::uuid, NULL,
     30, 1, NULL, '手工清理过期分区（需 confirm）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 2) SUPER_ADMIN：本迁移的 11 个节点全给 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000710'::uuid),
                    ('20000000-0000-0000-0000-000000000711'::uuid),
                    ('20000000-0000-0000-0000-000000000712'::uuid),
                    ('20000000-0000-0000-0000-000000000713'::uuid),
                    ('20000000-0000-0000-0000-000000000714'::uuid),
                    ('20000000-0000-0000-0000-000000000715'::uuid),
                    ('20000000-0000-0000-0000-000000000716'::uuid),
                    ('20000000-0000-0000-0000-000000000730'::uuid),
                    ('20000000-0000-0000-0000-000000000731'::uuid),
                    ('20000000-0000-0000-0000-000000000732'::uuid),
                    ('20000000-0000-0000-0000-000000000733'::uuid)) AS m(id)
 WHERE r.role_code = 'SUPER_ADMIN'
   AND r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 3) AUDITOR（若已存在）：变更日志页 + list/export，不给 clean ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000730'::uuid),
                    ('20000000-0000-0000-0000-000000000731'::uuid),
                    ('20000000-0000-0000-0000-000000000732'::uuid)) AS m(id)
 WHERE r.role_code = 'AUDITOR'
   AND r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 4) 自检（失败即迁移失败） ----------
DO $$
DECLARE
    node_cnt          int;
    super_missing     int;
    leaked            int;
    help_page         uuid := '20000000-0000-0000-0000-000000000740'::uuid;
    help_read         uuid := '20000000-0000-0000-0000-000000000741'::uuid;
    ann_page          uuid := '20000000-0000-0000-0000-000000000700'::uuid;
    ann_btn           uuid := '20000000-0000-0000-0000-000000000701'::uuid;
BEGIN
    -- 4.1 本迁移的 11 个节点必须齐（…0710–…0716 七 + …0730–…0733 四）
    SELECT count(*) INTO node_cnt
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND (id BETWEEN '20000000-0000-0000-0000-000000000710'::uuid
                   AND '20000000-0000-0000-0000-000000000716'::uuid
         OR id BETWEEN '20000000-0000-0000-0000-000000000730'::uuid
                   AND '20000000-0000-0000-0000-000000000733'::uuid);
    IF node_cnt <> 11 THEN
        RAISE EXCEPTION 'V0085 自检失败：公告管理 + 变更日志节点应有 11 个，实际 %', node_cnt;
    END IF;

    -- 4.2 SUPER_ADMIN 必须持有全部 11 个（否则超管自己在角色树里看不到入口）
    SELECT count(*) INTO super_missing
      FROM sys_role r
     CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000710'::uuid),
                        ('20000000-0000-0000-0000-000000000711'::uuid),
                        ('20000000-0000-0000-0000-000000000712'::uuid),
                        ('20000000-0000-0000-0000-000000000713'::uuid),
                        ('20000000-0000-0000-0000-000000000714'::uuid),
                        ('20000000-0000-0000-0000-000000000715'::uuid),
                        ('20000000-0000-0000-0000-000000000716'::uuid),
                        ('20000000-0000-0000-0000-000000000730'::uuid),
                        ('20000000-0000-0000-0000-000000000731'::uuid),
                        ('20000000-0000-0000-0000-000000000732'::uuid),
                        ('20000000-0000-0000-0000-000000000733'::uuid)) AS m(id)
     WHERE r.role_code = 'SUPER_ADMIN'
       AND r.deleted_at IS NULL
       AND NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = r.id AND rm.menu_id = m.id
           );
    IF super_missing > 0 THEN
        RAISE EXCEPTION 'V0085 自检失败：SUPER_ADMIN 缺 % 个本迁移节点的授权', super_missing;
    END IF;

    -- 4.3 变更日志不得泄漏给普通角色（C8 验收第 1 条的硬约束）
    SELECT count(*) INTO leaked
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id
     WHERE rm.menu_id BETWEEN '20000000-0000-0000-0000-000000000730'::uuid
                          AND '20000000-0000-0000-0000-000000000733'::uuid
       AND r.deleted_at IS NULL
       AND r.role_code NOT IN ('SUPER_ADMIN', 'AUDITOR');
    IF leaked > 0 THEN
        RAISE EXCEPTION 'V0085 自检失败：有 % 条变更日志授权落在非 SUPER_ADMIN/AUDITOR 角色上', leaked;
    END IF;

    -- 4.4 用户侧入口（公告中心 / 帮助中心 / 两个 read 按钮）由 V0087、V0088 种下。
    --
    -- ⚠ 时序修正（2026-10-29 生产部署事故）：这两个迁移的**版本号大于 V0085**，
    --   按版本号升序执行时它们在 V0085 之后才跑，因此**首次部署时必然还不存在**。
    --   原实现在此硬断言它们存在 → 首次 `cargo run` 迁移直接 FATAL 失败
    --   （已复现：'V0085 自检失败：公告中心 ...0700 不存在（应由 V0087 种下）'）。
    --
    -- 现改为：**存在才做一致性校验，不存在则跳过**（首次部署的正常路径）。
    -- 若将来需要在 V0087/V0088 之后做回归校验，应当新增一个号更大的断言迁移，
    -- 而不是在本迁移里假设后序迁移已完成。
    IF EXISTS (SELECT 1 FROM sys_menu WHERE id = ann_page AND deleted_at IS NULL) THEN
        IF NOT EXISTS (SELECT 1 FROM sys_role_menu WHERE menu_id = ann_page) THEN
            RAISE EXCEPTION 'V0085 自检失败：公告中心 % 已存在但未授权给任何角色', ann_page;
        END IF;
    END IF;
    IF EXISTS (SELECT 1 FROM sys_menu WHERE id = ann_btn AND deleted_at IS NULL) THEN
        IF NOT EXISTS (SELECT 1 FROM sys_role_menu WHERE menu_id = ann_btn) THEN
            RAISE EXCEPTION 'V0085 自检失败：announcement:read 按钮 % 已存在但未授权', ann_btn;
        END IF;
    END IF;
    IF EXISTS (SELECT 1 FROM sys_menu WHERE id = help_page AND deleted_at IS NULL) THEN
        IF NOT EXISTS (SELECT 1 FROM sys_role_menu WHERE menu_id = help_page) THEN
            RAISE EXCEPTION 'V0085 自检失败：帮助中心 % 已存在但未授权给任何角色', help_page;
        END IF;
    END IF;
    IF EXISTS (SELECT 1 FROM sys_menu WHERE id = help_read AND deleted_at IS NULL) THEN
        IF NOT EXISTS (SELECT 1 FROM sys_role_menu WHERE menu_id = help_read) THEN
            RAISE EXCEPTION 'V0085 自检失败：help:read 按钮 % 已存在但未授权', help_read;
        END IF;
    END IF;
END
$$;
