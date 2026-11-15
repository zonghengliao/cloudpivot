-- ======================================================
-- V0087：公告用户侧读权限种子（`business:announcement:read`）+ 全员授予
--
-- 依据：docs/批次C-技术架构.md §2.8.1（用户侧权限码）、§5.5.1 A-20
--       「无 business:announcement:read **不存在**（全员都有，断言种子覆盖所有角色）」
-- 回滚：V0087__seed_announcement_read_permission.down.sql
--
-- ── 为什么 C2 必须自带一条种子迁移，而不是等 V0085 ──────────────
-- 架构 §5.3 把批次 C 的 20 个权限码统一放在 **V0085**（含帮助文档 / 变更日志的
-- 管理码与 4 个菜单）。C2 只交付**用户侧**，但这些接口的注解里已经写了
-- `permission = "business:announcement:read"`；若种子缺席，那么除超管（`role_code`
-- 短路）以外**所有人**打开公告栏都是 403 —— 用户侧不可用，等于没交付。
-- 因此本卡按「单用途种子迁移」的既有范式（同 V0068）补一条，只种**用户侧这 1 个码**：
--   ① 号段取预留带里的 **V0087**（V0085 的 20 码全集仍留给 C3/C4/C5 一次性落，
--      两者不冲突：V0085 将来 INSERT 时对同 id/同 auth_code 会 ON CONFLICT DO NOTHING）；
--   ② 菜单 UUID 段取 **...0700 / ...0701**（4xx 属批次 A、6xx 属批次 B，7xx 为本批用户侧
--      保留位），C3 的管理侧菜单请从 **...0710** 起排，避免撞号。
--
-- ── 授权策略：**所有未软删角色**都授 ────────────────────────────
-- 公告栏是「人人可见」的入口，与 `business:notice:mine`（V0045）同一授权口径。
-- 只授超管的话，普通员工登录后没有菜单节点 → 前端不渲染入口 → 公告等于没发。
--
-- ── 幂等 ──────────────────────────────────────────────────────
-- 固定 UUID + `ON CONFLICT DO NOTHING`：重复执行不报错，也不覆盖运维改名后的标题。
-- ======================================================

-- ---------- 1) 公告中心页面（menu_type = 2）+ 读按钮（menu_type = 3） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000700'::uuid, '公告中心', 2, 'business:announcement:read',
     '/announcement', NULL, 'business/announcement/index',
     70, 1, 'notification', '公告中心', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000701'::uuid, 'business:announcement:read', 3,
     'business:announcement:read',
     NULL, '20000000-0000-0000-0000-000000000700'::uuid, NULL,
     10, 1, NULL, '查看公告（列表 / 详情 / 已读 / 附件下载）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 2) 授予所有未软删角色（含 SUPER_ADMIN，便于角色树勾选状态正确） ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000700'::uuid),
                    ('20000000-0000-0000-0000-000000000701'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 3) 自检（失败即迁移失败） ----------
-- 两个节点必须在；且**每个**未软删角色都必须持有读按钮 —— 缺一个就是「有人登录后
-- 看不到公告栏」，宁可在迁移期炸掉，也不要等上线后被用户发现。
DO $$
DECLARE
    page_menu   uuid := '20000000-0000-0000-0000-000000000700'::uuid;
    read_menu   uuid := '20000000-0000-0000-0000-000000000701'::uuid;
    missing     int;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = page_menu AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0087 自检失败：公告中心菜单节点 % 不存在', page_menu;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = read_menu AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0087 自检失败：读权限按钮节点 % 不存在', read_menu;
    END IF;

    SELECT count(*) INTO missing
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND NOT EXISTS (
               SELECT 1 FROM sys_role_menu rm
                WHERE rm.role_id = r.id AND rm.menu_id = read_menu
           );

    IF missing > 0 THEN
        RAISE EXCEPTION 'V0087 自检失败：还有 % 个未软删角色没有 business:announcement:read 授权', missing;
    END IF;
END
$$;
