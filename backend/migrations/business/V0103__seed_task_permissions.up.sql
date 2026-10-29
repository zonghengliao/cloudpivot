-- ======================================================
-- V0103：工作任务管理 · 权限节点（`business:task:*` 5 个菜单行）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md §七（可见性）、§十一（7 个页面）、
--       §十三 T1 行（6 个接口）
-- 回滚：V0103__seed_task_permissions.down.sql
--
-- ── 为什么「建节点」与「授权」分在两个文件 ────────────────────
-- 本文件（版本化迁移）只建 **5 个菜单节点**；**授权**放在种子迁移
-- `migrations/seed/S__task_permissions.sql`。原因是一条实测出来的时序事实：
--
--   版本化迁移（V\\d+）**先**跑，种子迁移（S__*.sql）**后**跑（本次空库实测：
--   `schema_migrations.applied_seq` 最后两条是 menu_data=60 / role_data=61）。
--
-- 于是「在版本化迁移里 `INSERT … SELECT FROM sys_role`」在**全新库**上会命中
-- 0 行 —— 那时连 SUPER_ADMIN 角色都还没建（它由 S__role_data.sql 创建）。
-- 老库（升级场景）没这个问题，新装库有。把授权放进种子文件，两种场景都正确：
-- 种子在 role_data 之后执行，且在全新库里是该文件的第一次执行 → 一定会跑。
--
-- ── 5 个节点 ↔ 4 个权限码 ────────────────────────────────────
--   …0760 页面「工作任务」（menu_type=2，auth_code 用 read，前端路由 /task）
--   …0761 business:task:read    → GET /page、GET /{id}、GET /my
--   …0762 business:task:create  → POST /
--   …0763 business:task:update  → PUT /{id}
--   …0764 business:task:delete  → DELETE /{id}
--
-- ── 菜单 UUID 段（避免撞号，出过事）──────────────────────────
-- 本批已占用（`migrations/` 全库 grep 过，2026-10-29）：
--   …0400–…04xx 批次 A；…0620–…0624 批次 B；…0700/…0701 C2 用户侧；
--   …0710–…0716 C3 公告管理；…0730–…0733 变更日志；…0740–…0749 帮助文档；
--   …0750 内容管理目录；…0790 文档结构根（V0089）。
-- 任务管理取 **…0760–…0764**（…0760 段此前未使用）。
--
-- ── 幂等 ──────────────────────────────────────────────────
-- 固定 UUID + `ON CONFLICT (id) DO NOTHING`：重复执行不报错，也不覆盖运维改名后的标题。
-- ======================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000760'::uuid, '工作任务', 2, 'business:task:read',
     '/task', NULL, 'business/task/index',
     60, 1, 'task', '工作任务', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000761'::uuid, 'business:task:read', 3,
     'business:task:read',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     10, 1, NULL, '查看任务（分页 / 详情 / 我的任务）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000762'::uuid, 'business:task:create', 3,
     'business:task:create',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     20, 1, NULL, '新建任务（快速添加）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000763'::uuid, 'business:task:update', 3,
     'business:task:update',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     30, 1, NULL, '编辑任务（仅负责人 / 创建人）', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000000764'::uuid, 'business:task:delete', 3,
     'business:task:delete',
     NULL, '20000000-0000-0000-0000-000000000760'::uuid, NULL,
     40, 1, NULL, '删除任务（软删：负责人 / 创建人 / 管理员）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- 自检：5 个节点必须在（授权由种子迁移补，见文件头） ----------
DO $$
DECLARE
    page_menu uuid := '20000000-0000-0000-0000-000000000760'::uuid;
    found     int;
BEGIN
    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000760'::uuid,
                  '20000000-0000-0000-0000-000000000761'::uuid,
                  '20000000-0000-0000-0000-000000000762'::uuid,
                  '20000000-0000-0000-0000-000000000763'::uuid,
                  '20000000-0000-0000-0000-000000000764'::uuid)
       AND deleted_at IS NULL;
    IF found <> 5 THEN
        RAISE EXCEPTION 'V0103 自检失败：business:task:* 菜单节点只存在 % 个（应为 5）', found;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_menu WHERE id = page_menu AND parent_id IS NULL) THEN
        RAISE EXCEPTION 'V0103 自检失败：工作任务应是根菜单（parent_id 为空）';
    END IF;
END
$$;
