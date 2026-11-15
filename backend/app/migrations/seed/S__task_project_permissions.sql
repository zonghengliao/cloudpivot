-- ======================================================
-- 项目分组 · 权限授权种子（批次 C+ · T7c）
--
-- ⚠️ 文件名前缀必须是 `S__task_*` 而不是 `S__project_*`：种子按**文件名字典序**执行，
--    `S__project_permissions.sql` 会排在 `S__role_data.sql` **之前**（实测：空库启动后
--    `sys_role_menu` 里 0 条项目授权，而自检因为角色集为空而"真空通过"）。
--    T6b 的 `S__task_permissions.sql` 同样是因为排序才拿到角色，这里沿用同一命名段。
--
-- 节点由版本化迁移 `migrations/business/V0115__project_grouping.up.sql` 创建
-- （…0775 菜单 + …0776–…0779 四个按钮）。
-- 回滚：由 V0115 的 down 脚本统一清理（种子迁移没有独立 down）。
--
-- ── 为什么授权在种子而不是版本化迁移里 ────────────────────────
-- 版本化迁移先跑、种子后跑：空库上 V0115 执行时 `S__role_data.sql` 还没建出
-- SUPER_ADMIN 角色，`INSERT … SELECT FROM sys_role` 会命中 0 行（V0103 / V0113 都踩过）。
--
-- ── 授权口径（分两档，刻意不对称） ────────────────────────────
-- 1. `business:project:read` + `business:project:create` → **所有未软删角色**
--    项目页是人人要用的入口（谁都能建一个自己的分组）；数据隔离靠 scope，
--    不靠权限码。只授超管 = 普通员工登录后点不到「项目」= 模块没交付。
-- 2. `business:project:update` + `business:project:delete` → **只授 SUPER_ADMIN**
--    与 V0109 / V0113 的 assign / member / comment:delete 同口径：这是「改别人的东西」的
--    加项权限。项目**负责人与创建人**不需要这两个码 —— 行级关系在
--    `business/project/handler.rs::ensure_writable` 里放行，别把行级语义塞进权限码。
--
-- ── 幂等 ──────────────────────────────────────────────────
-- 固定 UUID + `NOT EXISTS` 预判 + `ON CONFLICT DO NOTHING`：重复执行不报错。
-- ======================================================

-- ---------- 1) 读 + 建：授予所有未软删角色 ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000775'::uuid),
                    ('20000000-0000-0000-0000-000000000776'::uuid),
                    ('20000000-0000-0000-0000-000000000777'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 2) 改 + 删：只授 SUPER_ADMIN ----------
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
  FROM sys_role r
 CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000778'::uuid),
                    ('20000000-0000-0000-0000-000000000779'::uuid)) AS m(id)
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND NOT EXISTS (
           SELECT 1 FROM sys_role_menu granted
            WHERE granted.role_id = r.id AND granted.menu_id = m.id
       )
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- 3) 自检（失败即启动失败） ----------
DO $$
DECLARE
    btn_read   uuid := '20000000-0000-0000-0000-000000000776'::uuid;
    btn_create uuid := '20000000-0000-0000-0000-000000000777'::uuid;
    btn_update uuid := '20000000-0000-0000-0000-000000000778'::uuid;
    btn_delete uuid := '20000000-0000-0000-0000-000000000779'::uuid;
    missing    int;
    super_lack int;
BEGIN
    -- 每个未软删角色都必须有 read + create
    SELECT count(*) INTO missing
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND EXISTS (
               SELECT 1
                 FROM (VALUES (btn_read), (btn_create)) AS b(id)
                WHERE NOT EXISTS (
                          SELECT 1 FROM sys_role_menu rm
                           WHERE rm.role_id = r.id AND rm.menu_id = b.id
                      )
           );
    IF missing > 0 THEN
        RAISE EXCEPTION 'S__project_permissions 自检失败：还有 % 个未软删角色缺少 business:project:read/create 授权', missing;
    END IF;

    -- 超管必须有 update + delete（否则「管理员能改能删」这条语义不成立）
    SELECT count(*) INTO super_lack
      FROM sys_role r
     WHERE r.deleted_at IS NULL
       AND r.role_code = 'SUPER_ADMIN'
       AND EXISTS (
               SELECT 1
                 FROM (VALUES (btn_update), (btn_delete)) AS b(id)
                WHERE NOT EXISTS (
                          SELECT 1 FROM sys_role_menu rm
                           WHERE rm.role_id = r.id AND rm.menu_id = b.id
                      )
           );
    IF super_lack > 0 THEN
        RAISE EXCEPTION 'S__project_permissions 自检失败：SUPER_ADMIN 缺少 business:project:update/delete 授权';
    END IF;
END
$$;
