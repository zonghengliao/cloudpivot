-- ======================================================
-- V0089：把批次 C 的 4 个入口页收进「内容管理」目录
--
-- 背景（2026-10-29 老大反馈）：
--   C 批次的「公告中心 / 公告管理 / 变更日志 / 帮助中心」在 V0085/V0087/V0088 中
--   以 `parent_id = NULL`（顶级）方式种下，导致：
--     · 菜单管理页里它们与「工作台 / system / support」同级，散落在顶级；
--     · 老大在「系统管理」目录下找不到，无法集中管理。
--
-- 本迁移：
--   1) 新建目录菜单 `内容管理`（menu_type=1, path=/content, component=Layout）
--   2) 把上述 4 个入口的 parent_id 指向它
--   3) 主动清除菜单树缓存键（否则前端要干等 TTL 328 秒才看到，部署时踩过这个坑）
--
-- 幂等：全部使用 ON CONFLICT / WHERE 条件，重复执行安全。
-- 回滚：V0089__group_content_menus.down.sql（把 parent_id 还原为 NULL 并删除该目录）
-- ======================================================

-- ── 1. 新建「内容管理」目录 ─────────────────────────────
INSERT INTO sys_menu (
    id, name, menu_type, auth_code, path, parent_id, component,
    sort_order, status, icon, title,
    is_frame, is_cache, is_visible, module_code,
    created_at, updated_at
) VALUES (
    '20000000-0000-0000-0000-000000000790',
    'content',
    1,                      -- 目录
    '',                     -- 目录本身不需要权限码
    '/content',
    NULL,                   -- 顶级
    'Layout',               -- 目录固定用 Layout
    70,                     -- 与「通用底座」同档，排在业务区
    1,
    'file',
    '内容管理',
    FALSE, FALSE, TRUE,
    'business',
    now(), now()
)
ON CONFLICT (id) DO NOTHING;

-- ── 2. 4 个入口页挂到该目录下 ────────────────────────────
UPDATE sys_menu
   SET parent_id  = '20000000-0000-0000-0000-000000000790',
       updated_at = now()
 WHERE id IN (
        '20000000-0000-0000-0000-000000000700',   -- 公告中心
        '20000000-0000-0000-0000-000000000710',   -- 公告管理
        '20000000-0000-0000-0000-000000000730',   -- 变更日志
        '20000000-0000-0000-0000-000000000740'    -- 帮助中心
       )
   AND deleted_at IS NULL;

-- ── 3. 自检：目录存在 + 4 个子菜单都挂上 ──────────────────
DO $$
DECLARE
    dir_cnt   int;
    child_cnt int;
BEGIN
    SELECT count(*) INTO dir_cnt
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000790'
       AND deleted_at IS NULL;
    IF dir_cnt <> 1 THEN
        RAISE EXCEPTION 'V0089 自检失败：内容管理目录未能创建';
    END IF;

    SELECT count(*) INTO child_cnt
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000000790'
       AND deleted_at IS NULL;
    IF child_cnt <> 4 THEN
        RAISE EXCEPTION 'V0089 自检失败：应有 4 个子菜单挂到内容管理，实际 %', child_cnt;
    END IF;
END
$$;

-- ── 4. 清菜单树缓存 ──────────────────────────────────────
-- 说明：迁移里清缓存需要 pg 侧能访问 Redis；本环境无 redis_fdw，
--       因此这里只做「标记提醒」，实际清理由部署脚本 / 运维执行：
--         redis-cli del cloudpivot:menu:tree:all
-- 已在本迁移的 .down.sql 与部署流程中注明。
-- （不写 TODO 空块，避免误导；见 docs/批次C-技术架构.md §5.3 部署注记）
